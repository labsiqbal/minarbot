use super::{error, Result};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;

pub const REPOSITORY: &str = "ghcr.io/labsiqbal/minarbot-computer";

#[derive(Clone, Debug)]
pub(super) enum Runtime {
    Podman(PathBuf),
    Docker {
        binary: PathBuf,
        socket: PathBuf,
        rootless: bool,
    },
}

pub(super) fn program(names: &[&str]) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    names
        .iter()
        .map(|name| Path::new("/usr/bin").join(name))
        .find(|path| {
            path.metadata()
                .is_ok_and(|stat| stat.is_file() && stat.permissions().mode() & 0o111 != 0)
        })
}

async fn output(mut command: Command, args: &[&str]) -> Option<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(4),
        command
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

impl Runtime {
    fn command(&self) -> Command {
        let mut command = match self {
            Self::Podman(binary) => {
                let mut command = Command::new(binary);
                command.arg("--remote=false");
                command
            }
            Self::Docker { binary, socket, .. } => {
                let mut command = Command::new(binary);
                command
                    .arg("--host")
                    .arg(format!("unix://{}", socket.display()))
                    .env_remove("DOCKER_CONTEXT");
                command
            }
        };
        command.kill_on_drop(true);
        command
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Podman(_) => "Podman",
            Self::Docker { .. } => "Docker",
        }
    }

    pub async fn podman() -> Option<Self> {
        let runtime = Self::Podman(program(&["podman"])?);
        (output(
            runtime.command(),
            &["info", "--format", "{{.Host.Security.Rootless}}"],
        )
        .await
        .as_deref()
            == Some("true"))
        .then_some(runtime)
    }

    pub async fn detect() -> Option<Self> {
        if let Some(podman) = Self::podman().await {
            return Some(podman);
        }
        let binary = program(&["docker"])?;
        let rootless_socket = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .map(|path| path.join("docker.sock"));
        for socket in rootless_socket
            .into_iter()
            .chain([PathBuf::from("/var/run/docker.sock")])
        {
            use std::os::unix::fs::FileTypeExt;
            if !socket.is_absolute()
                || !socket
                    .metadata()
                    .is_ok_and(|stat| stat.file_type().is_socket())
            {
                continue;
            }
            let mut runtime = Self::Docker {
                binary: binary.clone(),
                socket,
                rootless: false,
            };
            if let Some(info) = output(
                runtime.command(),
                &["info", "--format", "{{json .SecurityOptions}}"],
            )
            .await
            {
                if let Self::Docker { rootless, .. } = &mut runtime {
                    *rootless = info.contains("name=rootless");
                }
                return Some(runtime);
            }
        }
        None
    }

    pub async fn has_image(&self, image: &str) -> bool {
        output(
            self.command(),
            &[
                "image",
                "inspect",
                "--format",
                "{{json .RepoDigests}}",
                image,
            ],
        )
        .await
        .and_then(|output| serde_json::from_str::<Vec<String>>(&output).ok())
        .is_some_and(|digests| digests.iter().any(|digest| digest == image))
    }

    pub async fn pull(&self, image: &str) -> Result<()> {
        let status = tokio::time::timeout(
            Duration::from_secs(30 * 60),
            self.command()
                .args(["pull", image])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status(),
        )
        .await;
        if !status.is_ok_and(|status| status.is_ok_and(|status| status.success())) {
            return Err(error(
                503,
                format!("{} could not download the Computer image.", self.label()),
            ));
        }
        if !self.has_image(image).await {
            return Err(error(
                503,
                "Downloaded Computer image failed digest verification.",
            ));
        }
        Ok(())
    }

    pub async fn remove(&self, name: &str) -> bool {
        // A successful filter lookup also proves the runtime is reachable if the container already exited.
        let filter = format!("name=^/?{name}$");
        output(self.command(), &["ps", "-aq", "--filter", &filter])
            .await
            .is_some_and(|ids| ids.is_empty())
            || output(self.command(), &["rm", "-f", name]).await.is_some()
            || output(self.command(), &["ps", "-aq", "--filter", &filter])
                .await
                .is_some_and(|ids| ids.is_empty())
    }

    pub fn launch(&self, name: &str, profile: &Path, proxy: &Path) -> Result<Command> {
        let dir = profile
            .parent()
            .ok_or_else(|| error(503, "Invalid desktop storage path."))?;
        let mut command = self.command();
        command.args([
            "run",
            "--rm",
            "-i",
            "--pull=never",
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--log-driver=none",
            "--tmpfs",
            "/tmp:rw,exec,mode=1777",
            "--tmpfs",
            "/run:rw,mode=755",
            "--shm-size=256m",
            "--hostname=minarbot-desktop",
            "--name",
            name,
        ]);
        let uid = rustix::process::geteuid().as_raw();
        let gid = rustix::process::getegid().as_raw();
        if matches!(self, Self::Podman(_)) {
            command.arg("--userns=keep-id");
        }
        let user = match self {
            Self::Docker { rootless: true, .. } => "0:0".into(),
            _ => format!("{uid}:{gid}"),
        };
        command.args(["--user", &user]);
        let proxy_dir = proxy
            .parent()
            .ok_or_else(|| error(503, "Invalid proxy path."))?;
        for (source, target) in [
            (dir.join("home"), "/home/desktop:Z"),
            (profile.to_owned(), "/home/desktop/chrome:Z"),
            (dir.join("x11"), "/run/minarbot:Z"),
            (proxy_dir.to_owned(), "/run/minarbot-proxy:ro,z"),
        ] {
            let path = minarbot_launcher::files::absolute(&source)
                .map_err(|_| error(503, "Invalid desktop mount path."))?;
            let path = path
                .to_str()
                .filter(|path| !path.contains(':'))
                .ok_or_else(|| error(503, "Invalid desktop mount path."))?;
            // SELinux needs private labels on agent mounts and a shared label on the proxy.
            command.args(["--volume", &format!("{path}:{target}")]);
        }
        Ok(command)
    }
}
