use super::*;
use rustix::fs::{mkdirat, open, openat, Mode, OFlags, CWD};
use std::{
    os::fd::OwnedFd,
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(Failure::storage)
}
pub fn root(state: &AppState) -> Result<PathBuf> {
    if let Some(root) = &state.config.data.project_root {
        return Ok(PathBuf::from(root));
    }
    Ok(home()?.join("Minarbot/Projects"))
}
fn contains(parent: &Path, child: &Path) -> bool {
    child.starts_with(parent)
}
pub fn validate(state: &AppState, value: &Value, browse: bool, existing: bool) -> Result<PathBuf> {
    let s = value
        .as_str()
        .filter(|s| {
            s.len() <= 4096
                && s.starts_with('/')
                && !s.chars().any(|c| c < ' ' || c == '\u{7f}' || c == '\\')
        })
        .ok_or_else(|| error(400, "Use a normalized absolute folder path."))?;
    let path = PathBuf::from(s);
    let normalized = path.components().collect::<PathBuf>();
    if normalized != path
        || normalized.to_str() != Some(s)
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(error(400, "Use a normalized absolute folder path."));
    }
    let home = home()?;
    let data = state
        .config
        .database
        .parent()
        .ok_or_else(Failure::storage)?;
    let protected = [
        std::fs::canonicalize(data).unwrap_or_else(|_| data.to_path_buf()),
        home.join(".ssh"),
        home.join(".claude"),
        home.join(".hermes"),
        home.join(".config"),
        home.join(".local/share/minarbot"),
        home.join(".aws"),
        home.join(".gnupg"),
    ];
    if !contains(&home, &path)
        || !browse && path == home
        || path
            .strip_prefix(&home)
            .ok()
            .and_then(|p| p.components().next())
            .is_some_and(|c| c.as_os_str().to_string_lossy().starts_with(".codex"))
        || protected
            .iter()
            .any(|p| contains(p, &path) || !browse && contains(&path, p))
    {
        return Err(error(400,"Folder must stay inside Owner home and outside system, credential, and Minarbot data folders."));
    }
    if existing {
        open_directory(&path)
            .map_err(|_| error(400, "Folder is unavailable or contains a symbolic link."))?;
    }
    Ok(path)
}
pub fn open_directory(path: &Path) -> std::result::Result<OwnedFd, rustix::io::Errno> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut fd = open("/", flags, Mode::empty())?;
    for part in path.components() {
        if let Component::Normal(name) = part {
            fd = openat(&fd, name, flags, Mode::empty())?;
        }
    }
    Ok(fd)
}
pub fn create(state: &AppState, path: &Path) -> Result<()> {
    validate(state, &json!(path), false, false)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut fd = openat(CWD, "/", flags, Mode::empty()).map_err(|_| Failure::storage())?;
    for part in path.components() {
        if let Component::Normal(name) = part {
            match mkdirat(&fd, name, Mode::from_raw_mode(0o700)) {
                Ok(()) => (),
                Err(rustix::io::Errno::EXIST) => (),
                Err(_) => return Err(error(400, "Folder is unavailable.")),
            }
            fd = openat(&fd, name, flags, Mode::empty())
                .map_err(|_| error(400, "Folder contains a symbolic link or is unavailable."))?;
        }
    }
    init_repository(&fd);
    Ok(())
}
fn init_repository(fd: &OwnedFd) {
    for args in [
        vec!["init", "-q", "--template=", "-b", "main"],
        vec![
            "-c",
            "user.name=Minarbot",
            "-c",
            "user.email=projects@minarbot.local",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "Start project",
        ],
    ] {
        let Ok((mut command, path)) = crate::platform::git_command(fd) else {
            return;
        };
        let child = command
            .args(args)
            .current_dir(&path)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &path)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LANG", "C.UTF-8")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else {
            return;
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        return;
                    }
                    break;
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }
            }
        }
    }
}
pub fn browse(state: &AppState, path: Option<&str>) -> Result<Value> {
    let path = validate(
        state,
        &json!(path.map(PathBuf::from).unwrap_or(home()?)),
        true,
        true,
    )?;
    let fd = open_directory(&path).map_err(|_| error(400, "Folder is unavailable."))?;
    let mut dirs = Vec::new();
    for entry in
        crate::operator_file::entries(&fd).map_err(|_| error(400, "Folder is unavailable."))?
    {
        let (name, kind) = entry.map_err(|_| error(400, "Folder is unavailable."))?;
        if kind == rustix::fs::FileType::Directory {
            let name = name.to_string_lossy().into_owned();
            let child = path.join(&name);
            if validate(state, &json!(child), false, true).is_ok() {
                dirs.push(json!({"name":name,"path":child.to_string_lossy()}));
            }
        }
    }
    dirs.sort_by(|a, b| {
        string(a, "name")
            .to_lowercase()
            .cmp(&string(b, "name").to_lowercase())
    });
    dirs.truncate(200);
    Ok(
        json!({"path":path,"parent":if path==home()?{None}else{path.parent().map(|v|v.to_string_lossy().into_owned())},"folders":dirs}),
    )
}
