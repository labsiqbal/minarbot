use crate::{data::providers::format_timeout, operator_file, web::Failure, AppState};
use rusqlite::OptionalExtension;
use rustix::fs::{mkdirat, Mode};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    fs::{self, File},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as SyncMutex},
    time::{Duration, Instant},
};
use tokio::{
    sync::{watch, Mutex},
    time::Instant as ClockInstant,
};

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub harness: Option<Value>,
    pub test_harness_bridge: Option<Bridge>,
    pub test_pdf_bridge: Option<Bridge>,
    #[serde(skip)]
    pub test_mode: bool,
    #[serde(skip)]
    pub environment: HashMap<String, String>,
    #[serde(skip)]
    pub cache: Arc<Mutex<Option<(Instant, Value)>>>,
    #[serde(skip)]
    pub revocations: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    #[serde(skip)]
    pub stopping_processes: Arc<std::sync::Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    #[serde(skip)]
    pub homes: Arc<Mutex<HashMap<String, super::store::Home>>>,
    #[serde(skip)]
    pub(super) model_catalog: super::model_catalog::Cache,
    #[serde(skip)]
    clocks: Arc<SyncMutex<HashMap<String, TurnClock>>>,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Bridge {
    pub origin: String,
    pub token: String,
}

const HOMES_SUFFIX: &str = ".harness-homes";
pub(super) const SESSION_PREFIX: &str = "minarbot-session-";

pub(super) fn homes_root(database: &Path) -> std::io::Result<PathBuf> {
    let name = database
        .file_name()
        .ok_or_else(|| std::io::Error::other("Database path has no file name"))?;
    let data = database
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok(minarbot_launcher::files::sidecar(
        &data.canonicalize()?.join(name),
        HOMES_SUFFIX,
    ))
}

fn session_name(name: &str) -> bool {
    name.strip_prefix(SESSION_PREFIX)
        .or_else(|| name.strip_prefix("minarbot-session-"))
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id))
}

pub(crate) fn prepare_homes(database: &Path) -> crate::db::Result<File> {
    let root = homes_root(database)?;
    let name = root.file_name().ok_or("Invalid harness homes path")?;
    let data = operator_file::open_directory(root.parent().ok_or("Invalid harness homes path")?)?;
    match mkdirat(&data, name, Mode::RWXU) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
        Err(error) => return Err(error.into()),
    }
    let directory = operator_file::directory_at(&data, name)?;
    if !operator_file::private(&directory) {
        return Err("Harness homes directory must be private and owned by the server user".into());
    }
    // Hold through shutdown so a second server on this database cannot sweep live homes.
    directory
        .try_lock()
        .map_err(|error| format!("Cannot lock harness homes directory: {error}"))?;
    directory.set_permissions(fs::Permissions::from_mode(0o700))?;
    for entry in operator_file::entries(&directory)? {
        let (name, kind) = entry?;
        if name.to_str().is_some_and(session_name) && kind == rustix::fs::FileType::Directory {
            operator_file::remove_directory_at(&directory, &name)?;
        }
    }
    Ok(directory)
}

#[derive(Clone)]
pub(super) struct TurnClock {
    deadlines: watch::Sender<Deadlines>,
    idle: Duration,
    max: Duration,
}

#[derive(Clone, Copy)]
struct Deadlines {
    idle: ClockInstant,
    max: ClockInstant,
    waiting_since: Option<ClockInstant>,
    waiters: usize,
}

impl Deadlines {
    fn next(self) -> ClockInstant {
        self.idle.min(self.max)
    }
}

impl TurnClock {
    fn new(idle: Duration, max: Duration, checkpoint: Option<(u64, u64)>) -> Self {
        let now = ClockInstant::now();
        let (active, idle_left) = checkpoint
            .map(|(active, idle_left)| {
                (
                    Duration::from_millis(active),
                    Duration::from_millis(idle_left),
                )
            })
            .unwrap_or((Duration::ZERO, idle));
        let (deadlines, _) = watch::channel(Deadlines {
            idle: now + idle.min(idle_left),
            max: now + max.saturating_sub(active),
            waiting_since: None,
            waiters: 0,
        });
        Self {
            deadlines,
            idle,
            max,
        }
    }

    pub(super) fn activity(&self) {
        self.deadlines.send_if_modified(|d| {
            let now = ClockInstant::now();
            if d.waiting_since.is_some() || now >= d.next() {
                return false;
            }
            d.idle = now + self.idle;
            true
        });
    }

    fn pause(&self) -> super::Result<CardWait> {
        let paused = self.deadlines.send_if_modified(|d| {
            let now = ClockInstant::now();
            if d.waiting_since.is_none() && now >= d.next() {
                return false;
            }
            d.waiters += 1;
            d.waiting_since.get_or_insert(now);
            true
        });
        if !paused {
            return Err(self.failure(*self.deadlines.borrow()));
        }
        Ok(CardWait {
            clock: Some(self.clone()),
        })
    }

    fn resume(&self) {
        self.deadlines.send_modify(|d| {
            d.waiters -= 1;
            if d.waiters == 0 {
                if let Some(started) = d.waiting_since.take() {
                    let elapsed = started.elapsed();
                    d.idle += elapsed;
                    d.max += elapsed;
                }
            }
        });
    }

    pub(super) async fn expired(&self) -> Failure {
        let mut deadlines = self.deadlines.subscribe();
        loop {
            let d = *deadlines.borrow_and_update();
            if d.waiting_since.is_some() {
                let _ = deadlines.changed().await;
            } else if ClockInstant::now() >= d.next() {
                return self.failure(d);
            } else {
                tokio::select! {
                    _ = deadlines.changed() => {},
                    _ = tokio::time::sleep_until(d.next()) => {},
                }
            }
        }
    }

    fn failure(&self, d: Deadlines) -> Failure {
        let limit = if d.max <= d.idle {
            format!("Reached the {} limit.", format_timeout(self.max))
        } else {
            format!("No activity for {}.", format_timeout(self.idle))
        };
        super::error(
            502,
            format!("{limit} Check workspace changes before retrying. This turn was not replayed."),
        )
    }
}

pub(super) struct ActiveClock<'a> {
    config: &'a Config,
    turn: &'a str,
    pub(super) clock: TurnClock,
}

impl<'a> ActiveClock<'a> {
    pub(super) fn new(state: &'a AppState, turn: &'a str) -> super::Result<Self> {
        let config = &state.config.harness;
        let checkpoint = {
            let db = state.database.lock().map_err(|_| Failure::storage())?;
            db.connection
                .query_row(
                    "SELECT active_ms,idle_remaining_ms FROM harness_card_time WHERE turn=?",
                    [turn],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
        };
        let clock = TurnClock::new(
            state.config.data.turn_idle_timeout(),
            state.config.data.turn_max_timeout(),
            checkpoint,
        );
        config
            .clocks
            .lock()
            .map_err(|_| Failure::storage())?
            .insert(turn.into(), clock.clone());
        Ok(Self {
            config,
            turn,
            clock,
        })
    }
}

impl Drop for ActiveClock<'_> {
    fn drop(&mut self) {
        if let Ok(mut clocks) = self.config.clocks.lock() {
            clocks.remove(self.turn);
        }
    }
}

pub(crate) struct CardWait {
    clock: Option<TurnClock>,
}

impl CardWait {
    pub(crate) fn start(state: &AppState, turn: &str) -> super::Result<Option<Self>> {
        let clock = state
            .config
            .harness
            .clocks
            .lock()
            .map_err(|_| Failure::storage())?
            .get(turn)
            .cloned();
        clock.map(|clock| clock.pause()).transpose()
    }

    // Question and Computer cards release the process. Save both budgets for the next launch.
    pub(crate) fn checkpoint(
        state: &AppState,
        db: &rusqlite::Connection,
        turn: &str,
    ) -> super::Result<Option<Self>> {
        let wait = Self::start(state, turn)?;
        if let Some(clock) = wait.as_ref().and_then(|wait| wait.clock.as_ref()) {
            let d = *clock.deadlines.borrow();
            let now = d.waiting_since.ok_or_else(Failure::storage)?;
            let active_ms = clock
                .max
                .saturating_sub(d.max.saturating_duration_since(now))
                .as_millis() as u64;
            let idle_remaining_ms = d.idle.saturating_duration_since(now).as_millis() as u64;
            db.execute(
                "INSERT INTO harness_card_time(turn,active_ms,idle_remaining_ms) VALUES(?,?,?) ON CONFLICT(turn) DO UPDATE SET active_ms=excluded.active_ms,idle_remaining_ms=excluded.idle_remaining_ms",
                rusqlite::params![turn, active_ms, idle_remaining_ms],
            )?;
        }
        Ok(wait)
    }

    pub(crate) fn keep_paused(mut self) {
        self.clock = None;
    }
}

impl Drop for CardWait {
    fn drop(&mut self) {
        if let Some(clock) = &self.clock {
            clock.resume();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_sweeps_only_session_directories() -> crate::db::Result<()> {
        let scratch = super::super::process::ProbeHome::new()?;
        let database = scratch.0.join("state.sqlite");
        let root = scratch.0.join("state.sqlite.harness-homes");
        let guard = prepare_homes(&database)?;
        assert_eq!(fs::metadata(&root)?.permissions().mode() & 0o777, 0o700);
        let stale = root.join(format!("{SESSION_PREFIX}{}", uuid::Uuid::new_v4()));
        let file = root.join(format!("{SESSION_PREFIX}{}", uuid::Uuid::new_v4()));
        let link = root.join(format!("{SESSION_PREFIX}{}", uuid::Uuid::new_v4()));
        let unrelated = root.join("minarbot-session-not-a-uuid");
        let outside = scratch
            .0
            .join(format!("{SESSION_PREFIX}{}", uuid::Uuid::new_v4()));
        fs::create_dir(&stale)?;
        fs::create_dir(&unrelated)?;
        fs::create_dir(&outside)?;
        fs::write(stale.join("credential"), "synthetic")?;
        fs::write(&file, "keep")?;
        std::os::unix::fs::symlink(&outside, &link)?;
        std::os::unix::fs::symlink(&outside, stale.join("outside"))?;

        assert!(prepare_homes(&database).is_err());
        assert!(stale.is_dir());
        guard.unlock()?;
        let _guard = prepare_homes(&database)?;
        assert!(!stale.exists());
        assert!(unrelated.is_dir());
        assert!(outside.is_dir());
        assert!(fs::symlink_metadata(link)?.is_symlink());
        assert_eq!(fs::read_to_string(file)?, "keep");
        Ok(())
    }

    #[test]
    fn databases_sharing_data_keep_separate_homes() -> crate::db::Result<()> {
        let scratch = super::super::process::ProbeHome::new()?;
        let owner = prepare_homes(&scratch.0.join("owner.sqlite"))?;
        let peer = prepare_homes(&scratch.0.join("peer.sqlite"))?;
        assert_ne!(
            homes_root(&scratch.0.join("owner.sqlite"))?,
            homes_root(&scratch.0.join("peer.sqlite"))?
        );
        drop((owner, peer));
        Ok(())
    }

    #[test]
    fn startup_rejects_symlink_or_public_homes_root() -> crate::db::Result<()> {
        let scratch = super::super::process::ProbeHome::new()?;
        let database = scratch.0.join("state.sqlite");
        let root = scratch.0.join("state.sqlite.harness-homes");
        let outside = scratch.0.join("outside");
        let stale = outside.join(format!("{SESSION_PREFIX}{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&stale)?;
        std::os::unix::fs::symlink(&outside, &root)?;
        assert!(prepare_homes(&database).is_err());
        assert!(stale.is_dir());

        fs::remove_file(&root)?;
        fs::rename(outside, &root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755))?;
        assert!(prepare_homes(&database).is_err());
        assert!(root
            .join(stale.file_name().ok_or("Missing session name")?)
            .is_dir());
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn harness_events_every_30_seconds_complete_after_three_minutes(
    ) -> crate::harness::Result<()> {
        let clock = TurnClock::new(Duration::from_secs(40), Duration::from_secs(3600), None);
        let start = ClockInstant::now();
        let runtime = async {
            for _ in 0..6 {
                tokio::time::sleep(Duration::from_secs(30)).await;
                clock.activity();
            }
        };
        tokio::select! {
            failure = clock.expired() => return Err(failure),
            () = runtime => {}
        }
        assert_eq!(start.elapsed(), Duration::from_secs(180));
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn silent_harness_hits_idle_deadline() -> crate::harness::Result<()> {
        let clock = TurnClock::new(Duration::from_secs(600), Duration::from_secs(3600), None);
        let start = ClockInstant::now();
        let failure = clock.expired().await;
        assert_eq!(start.elapsed(), Duration::from_secs(600));
        assert!(failure.message.starts_with("No activity for 10 min."));
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn approval_wait_preserves_idle_and_wall_time() -> crate::harness::Result<()> {
        let clock = TurnClock::new(Duration::from_secs(40), Duration::from_secs(60), None);
        let start = ClockInstant::now();
        let runtime = async {
            tokio::time::sleep(Duration::from_secs(20)).await;
            clock.activity();
            let approval = clock.pause()?;
            tokio::time::sleep(Duration::from_secs(120)).await;
            drop(approval);
            tokio::time::sleep(Duration::from_secs(30)).await;
            clock.activity();
            std::future::pending::<crate::harness::Result<()>>().await
        };
        let failure = tokio::select! {
            failure = clock.expired() => failure,
            result = runtime => {
                result?;
                panic!("runtime should hit its wall limit");
            }
        };
        assert_eq!(start.elapsed(), Duration::from_secs(180));
        assert!(failure.message.starts_with("Reached the 1 min limit."));
        Ok(())
    }
}
