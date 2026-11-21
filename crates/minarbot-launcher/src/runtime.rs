use crate::{files::remove, Result};
use std::{
    fs,
    os::unix::{fs::PermissionsExt, net::SocketAddr},
    path::{Path, PathBuf},
};
use tokio::net::UnixListener;

struct SocketPath {
    path: PathBuf,
    temporary: Option<PathBuf>,
}

impl Drop for SocketPath {
    fn drop(&mut self) {
        let _ = remove(&self.path);
        if let Some(directory) = &self.temporary {
            let _ = fs::remove_dir_all(directory);
        }
    }
}

pub struct ControlSocket {
    pub listener: UnixListener,
    path: SocketPath,
}

impl ControlSocket {
    pub fn bind(data: &Path) -> Result<Self> {
        let mut path = data.join("launcher.sock");
        let temporary = if cfg!(target_os = "macos") || SocketAddr::from_pathname(&path).is_err() {
            let directory = nix::unistd::mkdtemp("/tmp/minarbot-XXXXXX")?;
            path = directory.join("launcher.sock");
            Some(directory)
        } else {
            remove(&path)?;
            None
        };
        let path = SocketPath { path, temporary };
        let listener = UnixListener::bind(&path.path)?;
        fs::set_permissions(&path.path, fs::Permissions::from_mode(0o600))?;
        Ok(Self { listener, path })
    }

    pub fn path(&self) -> &Path {
        &self.path.path
    }

    /// Release cleanup may remove the file that this launcher started from.
    pub fn executable(&self) -> std::io::Result<PathBuf> {
        #[cfg(target_os = "linux")]
        {
            Ok(PathBuf::from("/proc/self/exe"))
        }
        #[cfg(target_os = "macos")]
        {
            let directory =
                self.path.temporary.as_ref().ok_or_else(|| {
                    std::io::Error::other("Launcher runtime directory is missing.")
                })?;
            let executable = directory.join("minarbot-launcher");
            fs::copy(std::env::current_exe()?, &executable)?;
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
            Ok(executable)
        }
    }
}

pub fn run_server(binary: &Path, parent: &str) -> Result<()> {
    use std::os::unix::process::CommandExt;
    #[cfg(target_os = "linux")]
    nix::sys::prctl::set_pdeathsig(nix::sys::signal::Signal::SIGTERM)?;
    if nix::unistd::getppid().as_raw().to_string() != parent {
        return Err("Launcher stopped before server start.".into());
    }
    let mut command = std::process::Command::new(binary);
    #[cfg(target_os = "macos")]
    command.env("MINARBOT_LAUNCHER_PID", parent);
    Err(command.exec().into())
}
