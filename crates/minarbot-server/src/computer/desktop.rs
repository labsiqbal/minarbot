use super::{
    browser_actions,
    browser_pipe::private_stdio,
    error,
    options::dimensions,
    proxy,
    runtime::Runtime,
    setup::{Backend, Setup},
    Result,
};
use crate::{config::Config, web::Failure};
use base64::{engine::general_purpose::STANDARD, Engine};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    net::UnixStream,
    process::{Child, Command},
    sync::{watch, Mutex as AsyncMutex, Semaphore},
};

pub fn directory(path: &Path) -> Result<()> {
    if let Ok(m) = fs::symlink_metadata(path) {
        if !m.is_dir() || m.is_symlink() {
            return Err(error(503, "Desktop storage must use real directories."));
        }
    }
    fs::create_dir_all(path)
        .map_err(|_| error(503, "Desktop storage must use real directories."))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|_| Failure::storage())
}
fn safe_read(path: &Path) -> Result<Vec<u8>> {
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
        .map_err(|_| error(503, "Desktop file is unavailable or unsafe."))?;
    let mut b = Vec::new();
    f.take(16 * 1024 * 1024)
        .read_to_end(&mut b)
        .map_err(|_| Failure::storage())?;
    Ok(b)
}
fn birth(pid: u32) -> Option<String> {
    crate::platform::process_identity(pid).map(|(_, start)| start)
}
pub fn sandbox(profile: &Path, dir: &Path, proxy: &Path, executable: &Path) -> Vec<String> {
    use super::desktop_supervisor::{BINARY, PROXY};
    let mut a = [
        "--die-with-parent",
        "--new-session",
        "--unshare-user",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-net",
        "--ro-bind",
        "/usr",
        "/usr",
        "--ro-bind",
        "/etc",
        "/etc",
        "--ro-bind",
        "/opt",
        "/opt",
        "--symlink",
        "usr/bin",
        "/bin",
        "--symlink",
        "usr/sbin",
        "/sbin",
        "--symlink",
        "usr/lib",
        "/lib",
        "--symlink",
        "usr/lib64",
        "/lib64",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--tmpfs",
        "/home",
    ]
    .map(str::to_owned)
    .to_vec();
    for (flag, from, to) in [
        ("--bind", dir.join("home"), "/home/desktop"),
        ("--bind", profile.to_owned(), "/home/desktop/chrome"),
    ] {
        a.extend([flag.into(), from.to_string_lossy().into(), to.into()]);
    }
    a.extend(["--dir".into(), "/run/minarbot".into()]);
    for (flag, from, to) in [
        ("--bind", dir.join("x11"), "/run/minarbot"),
        ("--bind", dir.join("x11"), "/tmp/.X11-unix"),
        ("--ro-bind", proxy.to_owned(), PROXY),
        ("--ro-bind", executable.to_owned(), BINARY),
    ] {
        a.extend([flag.into(), from.to_string_lossy().into(), to.into()]);
    }
    a.extend(
        [
            "--chdir",
            "/home/desktop",
            "--",
            BINARY,
            "--desktop-supervisor",
            PROXY,
        ]
        .map(str::to_owned),
    );
    a
}
struct Busy<'a>(&'a AtomicUsize);
impl<'a> Busy<'a> {
    fn new(value: &'a AtomicUsize) -> Self {
        value.fetch_add(1, Ordering::SeqCst);
        Self(value)
    }
}
impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
struct Helper {
    child: Option<Child>,
    input: Option<UnixStream>,
    output: Option<Lines<BufReader<UnixStream>>>,
    sequence: u64,
    container: Option<(Runtime, String)>,
}
struct Fixture {
    bridge: super::options::TestBridge,
    id: String,
}
pub struct Desktop {
    id: String,
    sensitive: AtomicBool,
    _profile_lock: Option<fs::File>,
    fixture: Option<Fixture>,
    helper: AsyncMutex<Helper>,
    gate: AsyncMutex<()>,
    slots: Semaphore,
    pub activity: AtomicI64,
    pub stopped: AtomicBool,
    pub starting: AtomicBool,
    busy: AtomicUsize,
    browser_epoch: AtomicU64,
    pub display: i64,
    jpeg: Mutex<Option<(Vec<u8>, Vec<u8>)>>,
    pub disconnected: watch::Sender<u64>,
}
pub struct Manager {
    pub(crate) craft: super::craft::Sessions,
    pub viewers: super::vnc::Viewers,
    pub hub: super::hub::Hub,
    pub metrics: Mutex<super::host_metrics::Metrics>,
    test_bridge: Option<super::options::TestBridge>,
    pub(super) setup: Setup,
    pub width: u32,
    pub height: u32,
    pub root: PathBuf,
    idle: i64,
    max: usize,
    owner: String,
    leases: Mutex<Option<Connection>>,
    desktops: Mutex<HashMap<String, Arc<Desktop>>>,
    lease_dir: PathBuf,
    proxy_target: Option<proxy::TestTarget>,
    proxy_path: OnceLock<PathBuf>,
    proxy_stop: watch::Sender<bool>,
    closing: AtomicBool,
    pub(super) always_on_gate: AsyncMutex<()>,
    pub controls: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    input_rates: Mutex<HashMap<String, (i64, u32)>>,
    input_slots: Mutex<HashMap<String, Arc<Semaphore>>>,
    starts: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    shared_profiles: Mutex<std::collections::HashSet<String>>,
    #[cfg(test)]
    launcher: Option<PathBuf>,
}
impl Manager {
    pub fn new(config: &Config) -> crate::db::Result<Self> {
        let d = config.computer.desktop.clone().unwrap_or_default();
        let (width, height) = dimensions(d.geometry.as_deref().unwrap_or("1280x800"))?;
        let root = d
            .data_dir
            .clone()
            .unwrap_or_else(|| {
                config
                    .database
                    .parent()
                    .unwrap_or(Path::new("."))
                    .to_owned()
            })
            .join("agent-desktops");
        let root = minarbot_launcher::files::absolute(&root).map_err(|error| error.to_string())?;
        let uid = rustix::process::geteuid().as_raw();
        let (stop, _) = watch::channel(false);
        Ok(Self {
            craft: Default::default(),
            viewers: super::vnc::Viewers::default(),
            hub: super::hub::Hub::default(),
            metrics: Mutex::new(super::host_metrics::Metrics::default()),
            test_bridge: if d.test_runtime {
                config.computer.test_computer_bridge.clone()
            } else {
                None
            },
            setup: Setup::new(d.enabled, config.harness.test_mode || cfg!(test)),
            width,
            height,
            root,
            idle: d.idle_ms.unwrap_or(900000),
            max: d.max.unwrap_or(2),
            owner: uuid::Uuid::new_v4().to_string(),
            leases: Mutex::new(None),
            desktops: Mutex::new(HashMap::new()),
            lease_dir: d
                .lease_dir
                .unwrap_or_else(|| std::env::temp_dir().join(format!("minarbot-desktop-{uid}"))),
            proxy_target: d.test_only_proxy_target,
            proxy_path: OnceLock::new(),
            proxy_stop: stop,
            closing: AtomicBool::new(false),
            always_on_gate: AsyncMutex::new(()),
            controls: Mutex::new(HashMap::new()),
            input_rates: Mutex::new(HashMap::new()),
            input_slots: Mutex::new(HashMap::new()),
            starts: Mutex::new(HashMap::new()),
            shared_profiles: Mutex::new(std::collections::HashSet::new()),
            #[cfg(test)]
            launcher: None,
        })
    }
    pub fn enabled(&self) -> bool {
        self.setup.backend().is_some()
    }
    pub(super) fn craft_proxy(&self) -> Result<&Path> {
        if !self.enabled() || self.closing.load(Ordering::SeqCst) {
            return Err(error(503, "Set up Computer to use Craft Anything."));
        }
        let mut leases = self.leases.lock().map_err(|_| Failure::storage())?;
        if leases.is_none() {
            *leases = Some(self.open_storage()?);
        }
        self.proxy_path
            .get()
            .map(PathBuf::as_path)
            .ok_or_else(Failure::storage)
    }
    fn open_storage(&self) -> Result<Connection> {
        if self.closing.load(Ordering::SeqCst) {
            return Err(error(503, "Computer is shutting down."));
        }
        directory(&self.lease_dir)?;
        let database = self.lease_dir.join("leases.sqlite");
        let db = Connection::open(&database)?;
        fs::set_permissions(database, fs::Permissions::from_mode(0o600))
            .map_err(|_| Failure::storage())?;
        db.execute_batch("PRAGMA busy_timeout=3000; CREATE TABLE IF NOT EXISTS leases(display INTEGER PRIMARY KEY,scope TEXT NOT NULL,agent TEXT NOT NULL,owner TEXT NOT NULL,pid INTEGER NOT NULL,birth TEXT NOT NULL,UNIQUE(scope,agent)) STRICT")?;
        let proxy_dir = self
            .lease_dir
            .join(format!("v-{}", uuid::Uuid::new_v4().simple()));
        directory(&proxy_dir)?;
        let path = proxy_dir.join("proxy.sock");
        let listener = (|| {
            let listener = tokio::net::UnixListener::bind(&path)?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
            Ok::<_, std::io::Error>(listener)
        })()
        .inspect_err(|_| {
            let _ = fs::remove_dir_all(proxy_dir);
        })
        .map_err(|_| Failure::storage())?;
        self.proxy_path.set(path).map_err(|_| Failure::storage())?;
        proxy::serve(
            listener,
            self.proxy_target.clone(),
            self.proxy_stop.subscribe(),
        );
        Ok(db)
    }
    pub fn control_gate(&self, agent: &str) -> Result<Arc<AsyncMutex<()>>> {
        Ok(self
            .controls
            .lock()
            .map_err(|_| Failure::storage())?
            .entry(agent.into())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone())
    }
    pub fn admit_input(&self, agent: &str, now: i64) -> Result<tokio::sync::OwnedSemaphorePermit> {
        let mut rates = self.input_rates.lock().map_err(|_| Failure::storage())?;
        let entry = rates.entry(agent.to_owned()).or_insert((now, 0));
        if now - entry.0 >= 1000 {
            *entry = (now, 0);
        }
        entry.1 = entry.1.saturating_add(1);
        if entry.1 > 30 {
            return Err(error(429, "Computer input rate exceeded."));
        }
        self.input_slots
            .lock()
            .map_err(|_| Failure::storage())?
            .entry(agent.to_owned())
            .or_insert_with(|| Arc::new(Semaphore::new(8)))
            .clone()
            .try_acquire_owned()
            .map_err(|_| error(429, "Computer input queue is full."))
    }
    pub fn get(&self, agent: &str) -> Result<Option<Arc<Desktop>>> {
        Ok(self
            .desktops
            .lock()
            .map_err(|_| Failure::storage())?
            .get(agent)
            .cloned())
    }
    pub fn status(&self, agent: &str, now: i64) -> Result<Value> {
        let d = self.get(agent)?;
        let state = d
            .as_ref()
            .map(|d| {
                if d.stopped.load(Ordering::SeqCst) {
                    "off"
                } else if d.starting.load(Ordering::SeqCst) {
                    "starting"
                } else if d.busy.load(Ordering::SeqCst) == 0
                    && now - d.activity.load(Ordering::SeqCst) >= 1000
                {
                    "idle"
                } else {
                    "running"
                }
            })
            .unwrap_or("off");
        let setup = self.setup.status()?;
        let mut status = json!({"available":setup["state"]=="ready","setup":setup,"state":state,
            "stopsInMs":d.map(|d|(d.activity.load(Ordering::SeqCst)+self.idle-now).max(0)),
            "width":self.width,"height":self.height});
        if !crate::platform::LINUX_SANDBOX {
            status["reason"] = crate::platform::COMPUTER_UNAVAILABLE.into();
        }
        Ok(status)
    }
    pub(super) fn at_capacity(&self, agent: &str, pinned: usize) -> Result<bool> {
        if pinned >= self.max {
            return Ok(true);
        }
        let leases = self.leases.lock().map_err(|_| Failure::storage())?;
        let Some(db) = leases.as_ref() else {
            return Ok(false);
        };
        let leases = crate::files::rows(
            db,
            "SELECT agent,pid,birth FROM leases WHERE scope=?",
            [self.root.to_string_lossy()],
        )?;
        let active = leases.iter().filter(|row| {
            row["agent"] != agent
                && birth(row["pid"].as_u64().unwrap_or(0) as u32).as_deref()
                    == row["birth"].as_str()
        });
        Ok(active.count() >= self.max)
    }
    fn reserve(&self, agent: &str) -> Result<i64> {
        let mut guard = self.leases.lock().map_err(|_| Failure::storage())?;
        if guard.is_none() {
            *guard = Some(self.open_storage()?);
        }
        let db = guard
            .as_mut()
            .ok_or_else(|| error(403, "Agent computer is disabled."))?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let leases = crate::files::rows(&tx, "SELECT * FROM leases", [])?;
        for r in leases {
            if birth(r["pid"].as_u64().unwrap_or(0) as u32).as_deref() != r["birth"].as_str() {
                tx.execute(
                    "DELETE FROM leases WHERE display=?",
                    [r["display"].as_i64()],
                )?;
            }
        }
        if crate::files::one(
            &tx,
            "SELECT 1 FROM leases WHERE scope=? AND agent=?",
            params![self.root.to_string_lossy(), agent],
        )?
        .is_some()
        {
            return Err(error(409, "Another app process owns this agent computer."));
        }
        let count: i64 = tx.query_row(
            "SELECT count(*) FROM leases WHERE scope=?",
            [self.root.to_string_lossy()],
            |r| r.get(0),
        )?;
        if count >= self.max as i64 {
            return Err(error(
                429,
                "Desktop limit reached. Stop another computer first.",
            ));
        }
        let mut selected = None;
        for display in 100..300 {
            if !Path::new(&format!("/tmp/.X{display}-lock")).exists()
                && !Path::new(&format!("/tmp/.X11-unix/X{display}")).exists()
                && crate::files::one(&tx, "SELECT 1 FROM leases WHERE display=?", [display])?
                    .is_none()
            {
                selected = Some(display);
                break;
            }
        }
        let display = selected.ok_or_else(|| error(429, "No free desktop display."))?;
        let pid = std::process::id();
        tx.execute(
            "INSERT INTO leases VALUES(?,?,?,?,?,?)",
            params![
                display,
                self.root.to_string_lossy(),
                agent,
                self.owner,
                pid,
                birth(pid).ok_or_else(Failure::storage)?
            ],
        )?;
        tx.commit()?;
        Ok(display)
    }
    fn release(&self, agent: &str) -> Result<()> {
        if let Some(db) = self.leases.lock().map_err(|_| Failure::storage())?.as_ref() {
            db.execute(
                "DELETE FROM leases WHERE scope=? AND agent=? AND owner=?",
                params![self.root.to_string_lossy(), agent, self.owner],
            )?;
        }
        Ok(())
    }
    pub fn load_browser_profiles(&self, db: &Connection) -> crate::db::Result<()> {
        let mut profiles = db.prepare("SELECT agent FROM computer_shared_profiles")?;
        for id in profiles.query_map([], |row| row.get::<_, String>(0))? {
            self.set_shared_profile(&id?, true)
                .map_err(|error| error.message)?;
        }
        Ok(())
    }
    pub fn set_shared_profile(&self, agent: &str, shared: bool) -> Result<()> {
        let mut agents = self
            .shared_profiles
            .lock()
            .map_err(|_| Failure::storage())?;
        if shared {
            agents.insert(agent.to_owned());
        } else {
            agents.remove(agent);
        }
        Ok(())
    }
    fn browser_profile(&self, agent: &str) -> Result<(PathBuf, Option<fs::File>)> {
        if !self
            .shared_profiles
            .lock()
            .map_err(|_| Failure::storage())?
            .contains(agent)
        {
            return Ok((self.root.join(agent).join("chrome"), None));
        }
        directory(&self.root)?;
        let guard = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(self.root.join("owner-chrome.lock"))
            .map_err(|_| Failure::storage())?;
        rustix::fs::flock(&guard, rustix::fs::FlockOperation::NonBlockingLockExclusive).map_err(
            |_| {
                error(
                    409,
                    "Another Computer is using the shared browser profile. Stop it first.",
                )
            },
        )?;
        Ok((self.root.join("owner-chrome"), Some(guard)))
    }
    fn prepare(&self, agent: &str, profile: &Path, display: i64) -> Result<()> {
        directory(&self.root)?;
        let dir = self.root.join(agent);
        for p in [
            &dir,
            profile,
            &dir.join("home"),
            &dir.join("x11"),
            &profile.join("Default"),
        ] {
            directory(p)?;
        }
        let preferences = profile.join("Default/Preferences");
        let mut v: Value = if fs::symlink_metadata(&preferences).is_ok() {
            serde_json::from_slice(&safe_read(&preferences)?)
                .map_err(|_| error(503, "Desktop file is unavailable or unsafe."))?
        } else {
            json!({})
        };
        for (k, x) in [
            ("signin", json!({"allowed":false})),
            // The supervisor restores tabs with their Minarbot IDs.
            ("session", json!({"restore_on_startup":5})),
            (
                "profile",
                json!({"password_manager_enabled":false,"exit_type":"Normal","exited_cleanly":true}),
            ),
            (
                "download",
                json!({"default_directory":"/home/desktop/Downloads","prompt_for_download":true}),
            ),
            ("custom_handlers", json!({"enabled":false})),
        ] {
            if !v[k].is_object() {
                v[k] = json!({});
            }
            for (key, value) in x.as_object().into_iter().flatten() {
                v[k][key] = value.clone();
            }
        }
        v["credentials_enable_service"] = false.into();
        let tmp = profile.join(format!("Default/Preferences-{}", uuid::Uuid::new_v4()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|_| Failure::storage())?;
        file.write_all(v.to_string().as_bytes())
            .map_err(|_| Failure::storage())?;
        fs::rename(&tmp, &preferences).map_err(|_| Failure::storage())?;
        for p in ["vnc.sock".into(), format!("X{display}")] {
            let _ = fs::remove_file(dir.join("x11").join(p));
        }
        Ok(())
    }
    pub async fn start(&self, agent: &str, now: i64) -> Result<Arc<Desktop>> {
        self.start_with_environment(agent, now, &HashMap::new())
            .await
    }
    async fn start_with_environment(
        &self,
        agent: &str,
        now: i64,
        environment: &HashMap<String, String>,
    ) -> Result<Arc<Desktop>> {
        if !crate::platform::LINUX_SANDBOX {
            return Err(error(503, crate::platform::COMPUTER_UNAVAILABLE));
        }
        let backend = self
            .setup
            .backend()
            .filter(|_| !self.closing.load(Ordering::SeqCst))
            .ok_or_else(|| error(403, "Set up Computer on this device first."))?;
        if crate::web::id(agent).is_none() {
            return Err(error(400, "Invalid agent ID."));
        }
        let gate = self
            .starts
            .lock()
            .map_err(|_| Failure::storage())?
            .entry(agent.to_owned())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone();
        let _starting = gate.lock().await;
        if let Some(d) = self.get(agent)? {
            if d.stopped.load(Ordering::SeqCst) {
                return Err(error(
                    409,
                    "Computer is stopping. Try again after it stops.",
                ));
            }
            return Ok(d);
        }
        let (profile, profile_lock) = self.browser_profile(agent)?;
        let dir = self.root.join(agent);
        let display = self.reserve(agent)?;
        let mut reservation = StartReservation {
            manager: self,
            agent,
            desktop: None,
            ready: false,
        };
        let proxy = self.proxy_path.get().ok_or_else(Failure::storage)?;
        let container = match &backend {
            Backend::Host => None,
            Backend::Container { runtime, .. } => {
                let name = format!(
                    "minarbot-computer-{:x}",
                    Sha256::digest(self.root.join(agent).as_os_str().as_encoded_bytes())
                );
                if !runtime.remove(&name).await {
                    return Err(error(
                        503,
                        "Previous Computer container could not be cleaned up.",
                    ));
                }
                Some((runtime.clone(), name))
            }
        };
        self.prepare(agent, &profile, display)?;
        if container.is_some() {
            directory(&dir.join("home").join("chrome"))?;
        }
        let launcher = Path::new("/usr/bin/bwrap");
        #[cfg(test)]
        let launcher = self.launcher.as_deref().unwrap_or(launcher);
        let geometry = format!("{}x{}", self.width, self.height);
        let display_env = format!("unix/:{display}");
        let mut command = if let (Backend::Container { image, .. }, Some((runtime, name))) =
            (backend, &container)
        {
            let mut command = runtime.launch(name, &dir, &profile, proxy)?;
            command.args([
                "--env",
                &format!("DISPLAY={display_env}"),
                "--env",
                &format!("MINARBOT_DESKTOP_GEOMETRY={geometry}"),
                &image,
            ]);
            command
        } else {
            let executable = std::env::current_exe()
                .map_err(|_| error(503, "Desktop supervisor is unavailable."))?;
            let mut command = Command::new(launcher);
            command
                .args(sandbox(&profile, &dir, proxy, &executable))
                .env_clear()
                .envs(environment)
                .envs([
                    ("PATH", "/usr/bin:/bin"),
                    ("HOME", "/home/desktop"),
                    ("USER", "minarbot"),
                    ("LANG", "C.UTF-8"),
                    ("XDG_CONFIG_HOME", "/home/desktop/.config"),
                    ("XDG_CACHE_HOME", "/home/desktop/.cache"),
                ])
                .env("DISPLAY", &display_env)
                .env("MINARBOT_DESKTOP_GEOMETRY", &geometry);
            command
        };
        command.stderr(Stdio::null()).kill_on_drop(true);
        let (child, input, output, fixture) = if let Some(bridge) = &self.test_bridge {
            let env = command
                .as_std()
                .get_envs()
                .filter_map(|(k, v)| {
                    Some((
                        k.to_string_lossy().into_owned(),
                        v?.to_string_lossy().into_owned(),
                    ))
                })
                .collect::<std::collections::BTreeMap<_, _>>();
            let id = uuid::Uuid::new_v4().to_string();
            bridge
                .call(
                    "desktop-launch",
                    json!({"id":id,"env":env,"profile":profile,"proxy":proxy}),
                )
                .await?;
            (
                None,
                None,
                None,
                Some(Fixture {
                    bridge: bridge.clone(),
                    id,
                }),
            )
        } else {
            let (input, output) = private_stdio(&mut command)
                .map_err(|_| error(503, "Desktop supervisor channel is unavailable."))?;
            let child = command.spawn().map_err(|_| error(503, if container.is_some() {
                "Computer container could not start. Check the container runtime."
            } else {
                "Desktop sandbox could not start. Install bwrap and enable unprivileged user namespaces."
            }))?;
            drop(command);
            (
                Some(child),
                Some(input),
                Some(BufReader::new(output).lines()),
                None,
            )
        };
        let (disconnected, _) = watch::channel(0);
        let d = Arc::new(Desktop {
            id: uuid::Uuid::new_v4().to_string(),
            sensitive: AtomicBool::new(false),
            _profile_lock: profile_lock,
            fixture,
            helper: AsyncMutex::new(Helper {
                child,
                input,
                output,
                sequence: 0,
                container,
            }),
            gate: AsyncMutex::new(()),
            slots: Semaphore::new(8),
            activity: AtomicI64::new(now),
            stopped: AtomicBool::new(false),
            starting: AtomicBool::new(true),
            busy: AtomicUsize::new(0),
            browser_epoch: AtomicU64::new(0),
            display,
            jpeg: Mutex::new(None),
            disconnected,
        });
        self.desktops
            .lock()
            .map_err(|_| Failure::storage())?
            .insert(agent.into(), d.clone());
        reservation.desktop = Some(d.clone());
        let ready = tokio::time::timeout(Duration::from_secs(65), async {
            if d.fixture.is_some() {
                return Ok(());
            }
            let mut h = d.helper.lock().await;
            if h.container.is_some() {
                let mut bytes = serde_json::to_vec(environment).map_err(|_| Failure::storage())?;
                bytes.push(b'\n');
                h.input
                    .as_mut()
                    .ok_or_else(Failure::storage)?
                    .write_all(&bytes)
                    .await
                    .map_err(|_| error(503, "Computer container stopped before becoming ready."))?;
            }
            let message = if h.container.is_some() {
                "Computer container stopped before becoming ready. Check the container runtime."
            } else {
                "Desktop sandbox stopped before becoming ready. Check bwrap and desktop programs."
            };
            let output = h.output.as_mut().ok_or_else(Failure::storage)?;
            while let Ok(Some(line)) = output.next_line().await {
                match line.as_str() {
                    "ready" => return Ok(()),
                    "failed" => break,
                    _ => {}
                }
            }
            Err(error(503, message))
        })
        .await
        .unwrap_or_else(|_| Err(error(503, "Computer startup timed out.")));
        if let Err(e) = ready {
            let _ = self.stop(agent).await;
            return Err(e);
        }
        d.starting.store(false, Ordering::SeqCst);
        d.activity.store(now, Ordering::SeqCst);
        reservation.ready = true;
        Ok(d)
    }
    pub async fn stop(&self, agent: &str) -> Result<()> {
        self.viewers.revoke(agent)?;
        let Some(d) = self.get(agent)? else {
            return Ok(());
        };
        d.stopped.store(true, Ordering::SeqCst);
        d.disconnected.send_modify(|v| *v += 1);
        let mut h = d.helper.lock().await;
        if !self
            .get(agent)?
            .is_some_and(|current| Arc::ptr_eq(&current, &d))
        {
            return Ok(());
        }
        let stopped = if let Some(fixture) = &d.fixture {
            tokio::time::timeout(
                Duration::from_secs(3),
                fixture
                    .bridge
                    .call("desktop-stop", json!({"id":fixture.id})),
            )
            .await
            .map(|v| v.map(|_| ()))
        } else {
            if let Some(mut input) = h.input.take() {
                let _ = input.shutdown().await;
            }
            tokio::time::timeout(
                Duration::from_secs(3),
                h.child.as_mut().ok_or_else(Failure::storage)?.wait(),
            )
            .await
            .map(|v| v.map(|_| ()).map_err(|_| Failure::storage()))
        };
        match stopped {
            Ok(result) => result?,
            Err(_) if h.container.is_some() => {
                if let Some((runtime, name)) = &h.container {
                    if !runtime.remove(name).await {
                        return Err(error(503, "Computer container is still stopping."));
                    }
                    if let Some(child) = &mut h.child {
                        let _ = child.kill().await;
                    }
                }
            }
            Err(_) => {
                return Err(error(
                    503,
                    "Computer is still stopping. Its display lease remains reserved.",
                ))
            }
        }
        for file in ["vnc.sock".into(), format!("X{}", d.display)] {
            let _ = fs::remove_file(self.root.join(agent).join("x11").join(file));
        }
        self.release(agent)?;
        self.desktops
            .lock()
            .map_err(|_| Failure::storage())?
            .remove(agent);
        Ok(())
    }
    async fn request(d: &Desktop, operation: &str, args: &[String]) -> Result<Vec<u8>> {
        if d.stopped.load(Ordering::SeqCst) {
            return Err(error(409, "Computer was stopped."));
        }
        if let Some(fixture) = &d.fixture {
            let value = fixture
                .bridge
                .call(
                    &format!("desktop-{operation}"),
                    json!({"id":fixture.id,"args":args}),
                )
                .await?;
            if d.stopped.load(Ordering::SeqCst) {
                return Err(error(409, "Computer was stopped."));
            }
            return STANDARD
                .decode(value.as_str().unwrap_or(""))
                .map_err(|_| Failure::storage());
        }
        let mut h = d.helper.lock().await;
        h.sequence += 1;
        let id = h.sequence;
        let text = format!("{}\n", json!({"id":id,"operation":operation,"args":args}));
        let work = async {
            h.input
                .as_mut()
                .ok_or_else(|| error(503, "Desktop helper stopped."))?
                .write_all(text.as_bytes())
                .await
                .map_err(|_| {
                    error(
                        503,
                        "Desktop helper stopped. Input outcome may be uncertain.",
                    )
                })?;
            loop {
                let line = h
                    .output
                    .as_mut()
                    .ok_or_else(Failure::storage)?
                    .next_line()
                    .await
                    .map_err(|_| Failure::storage())?
                    .ok_or_else(|| {
                        error(
                            503,
                            "Desktop helper stopped. Input outcome may be uncertain.",
                        )
                    })?;
                if line.len() > 16 * 1024 * 1024 {
                    return Err(error(
                        503,
                        "Desktop helper command failed. Input outcome may be uncertain.",
                    ));
                }
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if v["id"] == id {
                        if v["error"] == true {
                            return Err(error(
                                503,
                                "Desktop helper command failed. Input outcome may be uncertain.",
                            ));
                        }
                        return STANDARD
                            .decode(v["data"].as_str().unwrap_or(""))
                            .map_err(|_| Failure::storage());
                    }
                }
            }
        };
        let result = tokio::time::timeout(Duration::from_secs(12), work)
            .await
            .unwrap_or_else(|_| {
                Err(error(
                    503,
                    "Desktop helper timed out. Input outcome may be uncertain.",
                ))
            });
        if d.stopped.load(Ordering::SeqCst) {
            return Err(error(409, "Computer was stopped."));
        }
        result
    }
    pub fn screenshots_hidden(&self, agent: &str) -> Result<bool> {
        Ok(self
            .get(agent)?
            .is_some_and(|d| d.sensitive.load(Ordering::SeqCst)))
    }
    pub async fn secure_target(&self, agent: &str, origin: &str, target: &str) -> Result<String> {
        let d = self
            .get(agent)?
            .ok_or_else(|| error(409, "Open the page and focus its field first."))?;
        let _gate = d.gate.lock().await;
        let reply = Self::request(&d, "secret-target", &[origin.into(), target.into()]).await;
        if reply.ok().as_deref() != Some(&[1]) {
            return Err(error(409, "Focus an editable field on the requested site."));
        }
        Ok(d.id.clone())
    }
    pub async fn secure_fill(
        &self,
        agent: &str,
        origin: &str,
        target: Option<(&str, &str)>,
        reference: Option<&str>,
        value: String,
    ) -> Result<bool> {
        let Some(d) = self.get(agent)? else {
            return Ok(false);
        };
        let _gate = d.gate.lock().await;
        if target.is_some_and(|(desktop, _)| d.id != desktop) {
            return Ok(false);
        }
        let token = target.map_or("", |(_, token)| token);
        let already_hidden = d.sensitive.swap(true, Ordering::SeqCst);
        let mut args = vec![origin.into(), token.into(), value];
        if let Some(reference) = reference {
            args.extend([
                reference.into(),
                d.browser_epoch.load(Ordering::SeqCst).to_string(),
            ]);
        }
        let reply = Self::request(&d, "secret-fill", &args).await.map_err(|_| {
            error(
                503,
                "Secure fill outcome is uncertain. Check Computer before trying again.",
            )
        })?;
        d.activity.store(crate::config::now(), Ordering::SeqCst);
        match reply.as_slice() {
            [1] => Ok(true),
            [0] => {
                d.sensitive.store(already_hidden, Ordering::SeqCst);
                Ok(false)
            }
            _ => Err(error(
                503,
                "Secure fill outcome is uncertain. Check Computer before trying again.",
            )),
        }
    }
    pub async fn screenshot(&self, agent: &str, jpeg: bool) -> Result<Vec<u8>> {
        let d = self
            .get(agent)?
            .filter(|d| !d.stopped.load(Ordering::SeqCst) && !d.starting.load(Ordering::SeqCst))
            .ok_or_else(|| error(409, "Computer is not running."))?;
        let _slot = d
            .slots
            .try_acquire()
            .map_err(|_| error(429, "Computer is busy."))?;
        let _busy = Busy::new(&d.busy);
        let _gate = d.gate.lock().await;
        if d.sensitive.load(Ordering::SeqCst) {
            return Err(error(409, "Screenshots are hidden after secure fill. View Computer directly or stop it to start a new session."));
        }
        let bytes = Self::request(&d, "capture", &[]).await?;
        if jpeg {
            if let Some((old, encoded)) = d.jpeg.lock().map_err(|_| Failure::storage())?.as_ref() {
                if old == &bytes {
                    return Ok(encoded.clone());
                }
            }
        }
        let width = self.width;
        let height = self.height;
        let (bytes, encoded) = tokio::task::spawn_blocking(move || -> Result<_> {
            let mut reader = image::ImageReader::new(std::io::Cursor::new(&bytes))
                .with_guessed_format()
                .map_err(|_| Failure::storage())?;
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(width);
            limits.max_image_height = Some(height);
            reader.limits(limits);
            let image = reader.decode().map_err(|_| Failure::storage())?.resize(
                960,
                600,
                image::imageops::FilterType::Lanczos3,
            );
            let mut encoded = Vec::new();
            if jpeg {
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 72)
                    .encode_image(&image)
                    .map_err(|_| Failure::storage())?;
            } else {
                image
                    .write_to(
                        &mut std::io::Cursor::new(&mut encoded),
                        image::ImageFormat::Png,
                    )
                    .map_err(|_| Failure::storage())?;
            }
            Ok((bytes, encoded))
        })
        .await
        .map_err(|_| Failure::storage())??;
        if jpeg {
            *d.jpeg.lock().map_err(|_| Failure::storage())? = Some((bytes, encoded.clone()));
        }
        Ok(encoded)
    }
    pub(super) async fn browser_action(
        &self,
        agent: &str,
        action: browser_actions::Action,
        now: i64,
    ) -> Result<(Option<Vec<u8>>, Value)> {
        let d = self.start(agent, now).await?;
        d.activity.store(now, Ordering::SeqCst);
        let _slot = d
            .slots
            .try_acquire()
            .map_err(|_| error(429, "Computer is busy."))?;
        let _gate = d.gate.lock().await;
        let _busy = Busy::new(&d.busy);
        let request = browser_actions::Request {
            action,
            epoch: d.browser_epoch.load(Ordering::SeqCst),
        };
        let packet = serde_json::to_string(&request).map_err(|_| Failure::storage())?;
        let response = Self::request(&d, "browser", &[packet]).await;
        d.activity.store(crate::config::now(), Ordering::SeqCst);
        let reply: browser_actions::Reply =
            serde_json::from_slice(&response?).map_err(|_| Failure::storage())?;
        let image = reply
            .image
            .map(|image| STANDARD.decode(image).map_err(|_| Failure::storage()))
            .transpose()?;
        Ok((image, reply.result))
    }
    pub(super) fn invalidate_browser(&self, agent: &str) -> Result<()> {
        if let Some(d) = self.get(agent)? {
            d.browser_epoch.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
    pub async fn action(
        &self,
        agent: &str,
        name: &str,
        args: &Value,
        now: i64,
    ) -> Result<(Option<Vec<u8>>, Option<String>)> {
        let command = super::actions::validate(name, args, self.width, self.height)?;
        self.run_action(agent, name, command, now, &HashMap::new())
            .await
    }
    pub async fn action_with_environment(
        &self,
        agent: &str,
        name: &str,
        args: &Value,
        now: i64,
        environment: &HashMap<String, String>,
        mut cancelled: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<(Option<Vec<u8>>, Option<String>)> {
        let command = super::actions::validate(name, args, self.width, self.height)?;
        self.stop(agent).await?;
        let result = tokio::select! {
            biased;
            _ = &mut cancelled => Err(error(409, "Computer action was cancelled.")),
            result = self.run_action(agent, name, command, now, environment) => result,
        };
        self.stop(agent).await?;
        result
    }
    async fn run_action(
        &self,
        agent: &str,
        name: &str,
        command: Vec<String>,
        now: i64,
        environment: &HashMap<String, String>,
    ) -> Result<(Option<Vec<u8>>, Option<String>)> {
        let d = self.start_with_environment(agent, now, environment).await?;
        d.activity.store(now, Ordering::SeqCst);
        if name == "computer_screenshot" {
            return Ok((Some(self.screenshot(agent, false).await?), None));
        }
        let _slot = d
            .slots
            .try_acquire()
            .map_err(|_| error(429, "Computer is busy."))?;
        let _gate = d.gate.lock().await;
        let _busy = Busy::new(&d.busy);
        let result = async {
            if name == "computer_open_url" {
                for cmd in [
                    vec![
                        "search",
                        "--onlyvisible",
                        "--class",
                        super::desktop_supervisor::BROWSER_CLASS,
                        "windowactivate",
                        "--sync",
                    ]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                    vec!["key", "--clearmodifiers", "Control_L+l"]
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                    vec![
                        "type".into(),
                        "--clearmodifiers".into(),
                        "--delay".into(),
                        "0".into(),
                        "--".into(),
                        command[0].clone(),
                    ],
                    vec!["key", "--clearmodifiers", "Return"]
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                ] {
                    Self::request(&d, "input", &cmd).await?;
                }
                if d.sensitive.load(Ordering::SeqCst) {
                    return Ok((None, None));
                }
                let title = Self::request(
                    &d,
                    "input",
                    &["getactivewindow".into(), "getwindowname".into()],
                )
                .await
                .unwrap_or_default();
                Ok((
                    None,
                    Some(
                        String::from_utf8_lossy(&title)
                            .trim()
                            .trim_end_matches(" - Google Chrome")
                            .chars()
                            .take(300)
                            .collect(),
                    ),
                ))
            } else {
                if !command.is_empty() {
                    Self::request(&d, "input", &command).await?;
                }
                Ok((None, None))
            }
        }
        .await;
        d.activity.store(crate::config::now(), Ordering::SeqCst);
        result
    }
    pub async fn maintain(&self, now: i64, always_on: impl Fn(&str) -> Result<bool>) -> Result<()> {
        let rows = self
            .desktops
            .lock()
            .map_err(|_| Failure::storage())?
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<Vec<_>>();
        for (agent, d) in rows {
            let gate = self.control_gate(&agent)?;
            let Ok(_guard) = gate.try_lock() else {
                continue;
            };
            if !self
                .get(&agent)?
                .is_some_and(|current| Arc::ptr_eq(&current, &d))
            {
                continue;
            }
            let exited = if let Some(fixture) = &d.fixture {
                fixture
                    .bridge
                    .call("desktop-status", json!({"id":fixture.id}))
                    .await?
                    == true
            } else {
                d.helper.try_lock().ok().is_some_and(|mut h| {
                    h.child
                        .as_mut()
                        .is_some_and(|child| child.try_wait().ok().flatten().is_some())
                })
            };
            if exited
                || d.stopped.load(Ordering::SeqCst)
                || (!d.starting.load(Ordering::SeqCst)
                    && d.busy.load(Ordering::SeqCst) == 0
                    && now - d.activity.load(Ordering::SeqCst) >= self.idle
                    && !always_on(&agent)?)
            {
                self.stop(&agent).await?;
            }
        }
        Ok(())
    }
    pub async fn close(&self) {
        self.closing.store(true, Ordering::SeqCst);
        let _ = self.craft.clear();
        let agents = self
            .desktops
            .lock()
            .map(|m| m.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for agent in agents {
            let _ = self.stop(&agent).await;
        }
        let _ = self.proxy_stop.send(true);
        if let Some(p) = self.proxy_path.get().and_then(|path| path.parent()) {
            let _ = fs::remove_dir_all(p);
        }
    }
}

struct StartReservation<'a> {
    manager: &'a Manager,
    agent: &'a str,
    desktop: Option<Arc<Desktop>>,
    ready: bool,
}
impl Drop for StartReservation<'_> {
    fn drop(&mut self) {
        if self.ready {
            return;
        }
        if let Some(desktop) = &self.desktop {
            desktop.stopped.store(true, Ordering::SeqCst);
        } else {
            let _ = self.manager.release(self.agent);
        }
    }
}

#[cfg(test)]
pub(super) mod tests;
