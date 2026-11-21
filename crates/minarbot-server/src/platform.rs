use std::{collections::HashMap, io, os::fd::OwnedFd, path::PathBuf, process::Command};

pub const LINUX_SANDBOX: bool = cfg!(target_os = "linux");
pub const COMPUTER_UNAVAILABLE: &str = "Computer is unavailable on this platform.";

pub fn process_alive(pid: i32) -> bool {
    pid > 0
        && rustix::process::Pid::from_raw(pid).is_some_and(|pid| {
            rustix::process::test_kill_process(pid) != Err(rustix::io::Errno::SRCH)
        })
}

pub async fn launcher_exit() {
    #[cfg(target_os = "macos")]
    if let Some(pid) = std::env::var("MINARBOT_LAUNCHER_PID")
        .ok()
        .and_then(|pid| pid.parse::<i32>().ok())
    {
        while rustix::process::getppid().map(|parent| parent.as_raw_pid()) == Some(pid) {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        return;
    }
    std::future::pending().await
}

#[cfg(target_os = "linux")]
pub fn process_identity(pid: u32) -> Option<(u32, String)> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let mut fields = raw.rsplit_once(") ")?.1.split_ascii_whitespace();
    let parent = fields.nth(1)?.parse().ok()?;
    Some((parent, fields.nth(17)?.to_owned()))
}

#[cfg(target_os = "linux")]
pub fn processes() -> HashMap<u32, (u32, String)> {
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(|pid| process_identity(pid).map(|identity| (pid, identity)))
        .collect()
}

#[cfg(target_os = "macos")]
fn ps(selection: &[&str]) -> HashMap<u32, (u32, String)> {
    let output = Command::new("/bin/ps")
        .env("LC_ALL", "C")
        .args(selection)
        .args(["-o", "pid=", "-o", "ppid=", "-o", "lstart="])
        .output();
    let Ok(output) = output else {
        return HashMap::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_ascii_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let parent = fields.next()?.parse().ok()?;
            let start = fields.collect::<Vec<_>>().join(" ");
            (!start.is_empty()).then_some((pid, (parent, start)))
        })
        .collect()
}

#[cfg(target_os = "macos")]
pub fn process_identity(pid: u32) -> Option<(u32, String)> {
    ps(&["-p", &pid.to_string()]).remove(&pid)
}

#[cfg(target_os = "macos")]
pub fn processes() -> HashMap<u32, (u32, String)> {
    ps(&["-ax"])
}

#[cfg(target_os = "linux")]
pub fn git_command(directory: &OwnedFd) -> io::Result<(Command, PathBuf)> {
    use std::os::fd::AsRawFd;
    rustix::fs::fstat(directory)?;
    Ok((
        Command::new("git"),
        PathBuf::from(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            directory.as_raw_fd()
        )),
    ))
}

#[cfg(target_os = "macos")]
pub fn git_command(directory: &OwnedFd) -> io::Result<(Command, PathBuf)> {
    use std::os::unix::ffi::OsStrExt;
    let info = rustix::fs::fstat(directory)?;
    let path = rustix::fs::getpath(directory)?;
    let mut command = Command::new("/bin/sh");
    // Check the directory after chdir, while "." is pinned to the child's working inode.
    command.args([
        "-c",
        "test \"$(/usr/bin/stat -f %d:%i .)\" = \"$1\" || exit 1; shift; exec git \"$@\"",
        "minarbot-git",
        &format!("{}:{}", info.st_dev, info.st_ino),
    ]);
    Ok((command, std::ffi::OsStr::from_bytes(path.to_bytes()).into()))
}
