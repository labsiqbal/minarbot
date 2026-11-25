//! Dropping a test root kills the processes a test left behind.

use minarbot_testkit::{
    ensure,
    fakes::{self, Fake},
    reap::processes_under,
    Result, TestRoot,
};
use std::{os::unix::process::CommandExt, process::Command};

fn gone(pid: u32) -> bool {
    let state = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    state.is_empty() || state.contains(") Z ")
}

#[test]
fn dropping_the_root_kills_orphans_by_executable_and_working_directory() -> Result<()> {
    let root = TestRoot::new()?;
    let bin = root.dir("bin")?;
    let fake = fakes::install(Fake::Codex, &bin, "codex")?;
    // An installed fake (executable under the root), in its own process
    // group like a harness descendant, and a host program running in a
    // workspace (working directory under the root).
    let mut idle = Command::new(&fake)
        .arg("--fake-idle-child")
        .process_group(0)
        .spawn()?;
    let mut sleeper = Command::new("/bin/sleep")
        .arg("300")
        .current_dir(root.dir("workspace")?)
        .process_group(0)
        .spawn()?;
    let path = root.path().to_owned();
    let mut found = Vec::new();
    for _ in 0..100 {
        found = processes_under(&path);
        if found.len() >= 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    for child in [&idle, &sleeper] {
        let pid = i32::try_from(child.id())?;
        ensure!(
            found.contains(&pid),
            "{pid} not found under the root: {found:?}"
        );
    }
    drop(root);
    ensure!(gone(idle.id()), "idle child survived the root");
    ensure!(gone(sleeper.id()), "workspace process survived the root");
    idle.wait()?;
    sleeper.wait()?;
    Ok(())
}
