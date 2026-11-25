//! Kill processes a test left behind.
//!
//! A server stopped with SIGKILL (a test that fails early drops its
//! [`crate::Server`], or calls [`crate::Server::crash`]) cannot stop its
//! harness peers, and their descendants (`--fake-idle-child`) are
//! reparented to init and outlive the test. Every such process runs from the
//! test root (an installed fake binary) or in it (a workspace), so dropping
//! the root kills each process whose executable or working directory is
//! inside it.

use rustix::process::{kill_process, Pid, Signal};
use std::path::Path;

/// Process IDs whose `exe` or `cwd` lies inside `root`, except this process.
pub fn processes_under(root: &Path) -> Vec<i32> {
    let own = std::process::id();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<i32>().ok())
        .filter(|pid| u32::try_from(*pid).ok() != Some(own))
        .filter(|pid| {
            ["exe", "cwd"].iter().any(|link| {
                std::fs::read_link(format!("/proc/{pid}/{link}"))
                    .is_ok_and(|target| target.starts_with(root))
            })
        })
        .collect()
}

/// SIGKILL every process under `root`; repeats while processes remain, so
/// children forked meanwhile die too.
pub fn kill_under(root: &Path) {
    for _ in 0..5 {
        let pids = processes_under(root);
        if pids.is_empty() {
            return;
        }
        for pid in pids {
            if let Some(pid) = Pid::from_raw(pid) {
                let _ = kill_process(pid, Signal::KILL);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
