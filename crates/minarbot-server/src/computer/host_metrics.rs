use crate::{files, web::Failure, AppState};
use axum::{extract::Request, response::Response};
use serde_json::{json, Value};
use std::{
    fs,
    time::{Duration, Instant},
};

#[derive(Default)]
pub struct Metrics {
    previous: Option<(Instant, u64, u64, usize, u64)>,
    cached: Option<Value>,
    started: Option<Instant>,
}
#[cfg(target_os = "linux")]
fn memory(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        line.strip_prefix(key)?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()?
            .checked_mul(1024)
    })
}
#[cfg(any(test, target_os = "linux"))]
fn cgroup_limit(limits: [Option<&str>; 2], total: Option<u64>) -> Option<u64> {
    let limit = limits
        .into_iter()
        .flatten()
        .filter_map(|value| value.trim().parse::<u64>().ok())
        .min()?;
    Some(limit.min(total?))
}
#[cfg(target_os = "linux")]
fn constrained_memory(total: Option<u64>) -> Option<(u64, u64)> {
    let group = fs::read_to_string("/proc/self/cgroup").ok()?;
    let group = group.lines().find_map(|line| line.strip_prefix("0::"))?;
    let path = std::path::Path::new("/sys/fs/cgroup").join(group.trim_start_matches('/'));
    let limits = ["memory.max", "memory.high"].map(|name| fs::read_to_string(path.join(name)).ok());
    let limit = cgroup_limit(limits.each_ref().map(|value| value.as_deref()), total)?;
    let current = fs::read_to_string(path.join("memory.current"))
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    Some((limit, limit.saturating_sub(current)))
}
#[cfg(target_os = "linux")]
fn cpu() -> (u64, u64, usize) {
    let text = fs::read_to_string("/proc/stat").unwrap_or_default();
    let mut idle = 0;
    let mut total = 0;
    let mut cores = 0;
    for line in text
        .lines()
        .filter(|l| l.starts_with("cpu") && l.as_bytes().get(3).is_some_and(u8::is_ascii_digit))
    {
        let values: Vec<u64> = line
            .split_whitespace()
            .skip(1)
            .take(8)
            .map(|n| n.parse().unwrap_or(0))
            .collect();
        idle += values.get(3).copied().unwrap_or(0);
        total += values.iter().sum::<u64>();
        cores += 1;
    }
    (idle, total, cores)
}
struct HostSample {
    cpu: (u64, u64, usize),
    process_cpu: Option<u64>,
    memory: Option<u64>,
    available: Option<u64>,
    constrained: Option<(u64, u64)>,
    rss: Option<u64>,
}

#[cfg(target_os = "linux")]
fn host_sample() -> HostSample {
    let meminfo = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let total = memory(&meminfo, "MemTotal:");
    HostSample {
        cpu: cpu(),
        process_cpu: fs::read_to_string("/proc/self/schedstat")
            .ok()
            .and_then(|value| value.split_whitespace().next()?.parse().ok()),
        memory: total,
        available: memory(&meminfo, "MemAvailable:"),
        constrained: constrained_memory(total),
        rss: memory(
            &fs::read_to_string("/proc/self/status").unwrap_or_default(),
            "VmRSS:",
        ),
    }
}

#[cfg(target_os = "macos")]
fn host_sample() -> HostSample {
    let number = |command, args: &[&str]| {
        let output = std::process::Command::new(command)
            .args(args)
            .output()
            .ok()?;
        std::str::from_utf8(&output.stdout)
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()
    };
    HostSample {
        cpu: (
            0,
            0,
            std::thread::available_parallelism().map_or(1, |n| n.get()),
        ),
        process_cpu: None,
        memory: number("/usr/sbin/sysctl", &["-n", "hw.memsize"]),
        available: None,
        constrained: None,
        rss: number(
            "/bin/ps",
            &["-o", "rss=", "-p", &std::process::id().to_string()],
        )
        .and_then(|rss| rss.checked_mul(1024)),
    }
}
fn host_cpu_percent(before: (u64, u64, usize), after: (u64, u64, usize)) -> Option<f64> {
    let (before_idle, before_total, before_cores) = before;
    let (idle, total, cores) = after;
    let elapsed = total.checked_sub(before_total)?;
    let idle = idle.checked_sub(before_idle)?;
    (cores > 0 && cores == before_cores && elapsed > 0 && idle <= elapsed)
        .then(|| ((1.0 - idle as f64 / elapsed as f64) * 1000.0).round() / 10.0)
}
fn process_cpu_percent(before: u64, after: u64, elapsed: Duration) -> Option<f64> {
    after
        .checked_sub(before)
        .map(|used| (used as f64 / elapsed.as_nanos() as f64 * 1000.0).round() / 10.0)
}
fn disk(value: rustix::fs::StatVfs) -> Option<Value> {
    let (blocks, free, available, size) = (
        value.f_blocks as u128,
        value.f_bfree as u128,
        value.f_bavail as u128,
        value.f_frsize as u128,
    );
    if blocks == 0 || size == 0 || free < available || blocks < free {
        return None;
    }
    let (total, used, available, reserved) = (
        blocks * size,
        (blocks - free) * size,
        available * size,
        (free - available) * size,
    );
    let pressure = if available * 100 < total * 2 && available <= 1024u128.pow(3) {
        "critical"
    } else if available * 10 < total && available <= 5 * 1024u128.pow(3) {
        "warning"
    } else {
        "normal"
    };
    Some(
        json!({"total":total.to_string(),"available":available.to_string(),"used":used.to_string(),"reserved":reserved.to_string(),"percent":(used*1000/(used+available).max(1)) as f64/10.0,"pressure":pressure}),
    )
}
impl Metrics {
    fn collect(&mut self, state: &AppState) -> Result<Value, Failure> {
        let now = Instant::now();
        if self
            .previous
            .is_some_and(|p| now.duration_since(p.0) < Duration::from_secs(2))
        {
            if let Some(value) = &self.cached {
                return Ok(value.clone());
            }
        }
        let sample = host_sample();
        let (idle, total, cores) = sample.cpu;
        let interval = self
            .previous
            .map(|p| now.duration_since(p.0).as_millis() as u64);
        let percent = self
            .previous
            .and_then(|(_, before_idle, before_total, before_cores, _)| {
                host_cpu_percent(
                    (before_idle, before_total, before_cores),
                    (idle, total, cores),
                )
            });
        let process_percent = self.previous.and_then(|(before, _, _, _, cpu)| {
            process_cpu_percent(cpu, sample.process_cpu?, now.duration_since(before))
        });
        let available = sample.available;
        let total_memory = sample.memory;
        let constrained = sample.constrained;
        let available_memory = match (available, constrained) {
            (Some(host), Some((_, group))) => Some(host.min(group)),
            _ => available,
        };
        let rss = sample.rss.unwrap_or(0);
        let mut files_out = Vec::new();
        for (kind, suffix) in [
            ("database", ""),
            ("wal", "-wal"),
            ("shared-memory", "-shm"),
            ("rollback-journal", "-journal"),
            ("mcp-key", ".mcp-key"),
        ] {
            let path = minarbot_launcher::files::sidecar(&state.config.database, suffix);
            let (status, bytes) = match fs::symlink_metadata(path) {
                Ok(m) if m.is_file() => ("present", Some(m.len().to_string())),
                Ok(_) => ("not-regular", None),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => ("missing", None),
                Err(_) => ("unavailable", None),
            };
            files_out.push(json!({"kind":kind,"status":status,"bytes":bytes}));
        }
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        let page_size: u64 = db.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        let pages: u64 = db.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let free: u64 = db.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
        let value = json!({"sampledAt":files::timestamp(db,crate::config::now())?,"platform":std::env::consts::OS,"cpu":{"scope":"host","cores":cores,"availableParallelism":std::thread::available_parallelism().map(|n|n.get()).unwrap_or(1),"percent":percent,"intervalMs":interval},"memory":{"total":total_memory,"available":available.filter(|a|total_memory.is_some_and(|t|*a<=t)),"source":if crate::platform::LINUX_SANDBOX {"MemAvailable"} else {"sysctl"}},"process":{"rss":rss,"cpuPercent":process_percent,"memoryLimit":constrained.map(|v|v.0),"availableMemory":available_memory,"uptimeSeconds":now.duration_since(*self.started.get_or_insert(now)).as_secs()},"filesystem":state.config.database.parent().and_then(|p|rustix::fs::statvfs(p).ok()).and_then(disk),"storage":{"files":files_out,"databaseBytes":(page_size*pages).to_string(),"reusableBytes":(page_size*free).to_string()}});
        self.previous = Some((now, idle, total, cores, sample.process_cpu.unwrap_or(0)));
        self.cached = Some(value.clone());
        Ok(value)
    }
}
pub async fn handle(state: &AppState, request: Request) -> Result<Response, Failure> {
    if request.method() != axum::http::Method::GET {
        return Err(Failure::new(405, "invalid_request", "Method not allowed."));
    }
    files::origin(state, &request)?;
    if request.uri().query().is_some() {
        return Err(Failure::new(
            400,
            "invalid_request",
            "Host metrics do not accept paths or parameters.",
        ));
    }
    let value = state
        .computer
        .metrics
        .lock()
        .map_err(|_| Failure::storage())?
        .collect(state)?;
    Ok(crate::json_response(200, value))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn available_memory_does_not_substitute_free_memory() {
        assert_eq!(
            memory("MemFree: 10 kB\nMemAvailable: 20 kB\n", "MemAvailable:"),
            Some(20480)
        );
        assert_eq!(memory("MemAvailable: -1 kB\n", "MemAvailable:"), None);
    }

    #[test]
    fn disk_space_and_cpu_units_preserve_measurement_semantics() {
        let before = (100, 200, 2);
        assert_eq!(host_cpu_percent(before, (120, 300, 2)), Some(80.0));
        for after in [(100, 200, 2), (120, 300, 4), (90, 300, 2), (220, 300, 2)] {
            assert_eq!(host_cpu_percent(before, after), None);
        }
        assert_eq!(
            process_cpu_percent(10, 1_000_000_010, Duration::from_secs(1)),
            Some(100.0)
        );
        assert_eq!(
            process_cpu_percent(10, 2_000_000_010, Duration::from_secs(1)),
            Some(200.0)
        );
        assert_eq!(process_cpu_percent(10, 9, Duration::from_secs(1)), None);
        let space = |blocks, free, available, block_size| {
            disk(rustix::fs::StatVfs {
                f_bsize: block_size,
                f_frsize: block_size,
                f_blocks: blocks,
                f_bfree: free,
                f_bavail: available,
                f_files: 0,
                f_ffree: 0,
                f_favail: 0,
                f_fsid: 0,
                f_flag: rustix::fs::StatVfsMountFlags::empty(),
                f_namemax: 0,
            })
        };
        assert_eq!(
            space(100, 20, 10, 1024),
            Some(json!({
                "total":"102400","available":"10240","used":"81920","reserved":"10240","percent":88.8,"pressure":"normal"
            }))
        );
        assert_eq!(
            space(100, 10, 9, 1024).map(|v| v["pressure"].clone()),
            Some(json!("warning"))
        );
        assert_eq!(
            space(100, 10, 1, 1024).map(|v| v["pressure"].clone()),
            Some(json!("critical"))
        );
        assert!(space(0, 0, 0, 1).is_none());
        assert!(space(100, 1, 2, 1).is_none());
        assert_eq!(
            space(9_007_199_254_740_993, 2, 1, 4096).map(|v| v["total"].clone()),
            Some(json!("36893488147419107328"))
        );
    }

    #[test]
    fn cgroup_memory_reports_the_tightest_finite_limit() {
        let total = Some(32 * 1024 * 1024 * 1024);
        assert_eq!(cgroup_limit([Some("max"), None], total), None);
        assert_eq!(cgroup_limit([Some("-1"), Some("unavailable")], total), None);
        assert_eq!(
            cgroup_limit([Some("536870912\n"), Some("1073741824")], total),
            Some(536870912)
        );
        assert_eq!(
            cgroup_limit([Some("1073741824"), Some("536870912")], total),
            Some(536870912)
        );
        assert_eq!(cgroup_limit([Some("34359738369"), None], total), total);
        assert_eq!(cgroup_limit([Some("536870912"), None], None), None);
    }
}
