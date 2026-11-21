use super::*;
use crate::db::Result as TestResult;

const FIRST: &str = "10000000-0000-4000-8000-000000000001";
const SECOND: &str = "10000000-0000-4000-8000-000000000002";
const THIRD: &str = "10000000-0000-4000-8000-000000000003";

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> TestResult<Self> {
        let path = std::env::temp_dir().join(format!("bs-desk-{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn script(&self, body: &str) -> TestResult<PathBuf> {
        let path = self.0.join("launcher");
        fs::write(
            &path,
            format!("#!/bin/sh\ncd '{}' || exit 1\n{body}\n", self.0.display()),
        )?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        Ok(path)
    }

    fn manager(&self, launcher: &Path, max: usize) -> TestResult<Manager> {
        let config = serde_json::from_value(json!({
            "database": self.0.join("db.sqlite"),
            "desktop": {"enabled": true, "leaseDir": self.0.join("leases"), "idleMs": 1000, "max": max}
        }))?;
        let mut manager = Manager::new(&config)?;
        manager.launcher = Some(launcher.into());
        Ok(manager)
    }

    fn supervisor(&self) -> TestResult<PathBuf> {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(16, 10).write_to(&mut png, image::ImageFormat::Png)?;
        fs::write(self.0.join("png"), STANDARD.encode(png.into_inner()))?;
        self.script(
            r#"
printf '%s %s %s\n' "$$" "$DISPLAY" "${PROJECT_TOKEN:-unset}" >> launches
env | sort > environment
echo ready
sequence=0
while IFS= read -r line; do
    sequence=$((sequence + 1))
    printf '%s %s\n' "${PROJECT_TOKEN:-unset}" "$line" >> commands
    if [ -f pause ]; then
        while [ ! -f release ]; do sleep 0.02; done
    fi
    if [ -f die ]; then kill -KILL "$$"; fi
    if [ -f fail ]; then printf '{"id":%s,"error":true}\n' "$sequence"; continue; fi
    data=''
    case "$line" in *'"operation":"capture"'*) data=$(cat png);; esac
    printf '{"id":%s,"data":"%s"}\n' "$sequence" "$data"
done
if [ -f slow ]; then
    while [ ! -f exited ]; do sleep 0.02; done
fi
"#,
        )
    }

    async fn observed(&self, file: &str) -> TestResult<()> {
        tokio::time::timeout(Duration::from_secs(4), async {
            while !self.0.join(file).exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        Ok(())
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn check<T>(result: super::Result<T>) -> TestResult<T> {
    result.map_err(|error| format!("{error:?}").into())
}

#[tokio::test]
async fn sandbox_start_failure_releases_lease() -> TestResult<()> {
    let scratch = Scratch::new()?;
    let early_exit = scratch.script("exit 1")?;
    for path in [scratch.0.join("missing-bwrap"), early_exit] {
        let manager = scratch.manager(&path, 1)?;
        let error = manager
            .start(FIRST, 0)
            .await
            .err()
            .ok_or("launcher fails closed")?;
        assert_eq!(error.status, 503);
        assert!(error.message.contains("sandbox") && error.message.contains("bwrap"));
        assert_eq!(check(manager.status(FIRST, 0))?["state"], "off");
        check(manager.reserve(SECOND))?;
        check(manager.release(SECOND))?;
        manager.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn desktop_mounts_and_preferences_stay_private() -> TestResult<()> {
    let scratch = Scratch::new()?;
    let launcher = scratch.supervisor()?;
    let manager = scratch.manager(&launcher, 1)?;
    let outside = scratch.0.join("outside");
    fs::create_dir(&outside)?;
    fs::write(outside.join("Preferences"), r#"{"host":"private"}"#)?;
    let profile = manager.root.join(FIRST).join("chrome");
    fs::create_dir_all(&profile)?;
    let defaults = profile.join("Default");
    std::os::unix::fs::symlink(&outside, &defaults)?;
    assert_eq!(
        manager.start(FIRST, 0).await.err().map(|e| e.status),
        Some(503)
    );
    fs::remove_file(&defaults)?;
    fs::create_dir(&defaults)?;
    std::os::unix::fs::symlink(outside.join("Preferences"), defaults.join("Preferences"))?;
    assert_eq!(
        manager.start(FIRST, 0).await.err().map(|e| e.status),
        Some(503)
    );
    assert_eq!(
        fs::read(outside.join("Preferences"))?,
        br#"{"host":"private"}"#
    );
    assert!(!scratch.0.join("launches").exists());
    fs::remove_file(defaults.join("Preferences"))?;
    check(manager.start(FIRST, 0).await)?;
    let executable = std::env::current_exe()?;
    let args = sandbox(
        &profile,
        &manager.root.join(FIRST),
        &manager.proxy_path,
        &executable,
    );
    assert!(args.windows(3).any(|mount| mount[0] == "--ro-bind"
        && Path::new(&mount[1]) == executable
        && mount[2] == "/minarbot-server"));
    assert_eq!(
        &args[args.len() - 3..],
        [
            "/minarbot-server",
            "--desktop-supervisor",
            "/run/minarbot/proxy.sock"
        ]
    );
    let sources = args
        .windows(2)
        .filter(|a| matches!(a[0].as_str(), "--bind" | "--ro-bind"))
        .map(|a| PathBuf::from(&a[1]))
        .collect::<Vec<_>>();
    let agent = manager.root.join(FIRST);
    assert_eq!(
        sources,
        [
            "/usr".into(),
            "/etc".into(),
            "/opt".into(),
            agent.join("home"),
            profile,
            agent.join("x11"),
            agent.join("x11"),
            manager.proxy_path.clone(),
            executable
        ]
    );
    for flag in [
        "--unshare-user",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-net",
        "--new-session",
        "--die-with-parent",
    ] {
        assert!(args.iter().any(|arg| arg == flag));
    }
    let environment = fs::read_to_string(scratch.0.join("environment"))?;
    assert!(environment.lines().any(|line| line == "HOME=/home/desktop"));
    for key in [
        "DBUS_SESSION_BUS_ADDRESS=",
        "XDG_RUNTIME_DIR=",
        "OPENAI_API_KEY=",
        "XAUTHORITY=",
    ] {
        assert!(!environment.lines().any(|line| line.starts_with(key)));
    }
    let preferences: Value = serde_json::from_slice(&fs::read(defaults.join("Preferences"))?)?;
    assert_eq!(preferences["signin"]["allowed"], false);
    assert_eq!(preferences["credentials_enable_service"], false);
    assert_eq!(preferences["custom_handlers"]["enabled"], false);
    let typed = check(super::super::actions::validate(
        "computer_type",
        &json!({"text":"file:///home/private"}),
        1280,
        800,
    ))?;
    assert_eq!(
        typed.last().map(String::as_str),
        Some("file:///home/private")
    );
    manager.close().await;
    Ok(())
}

#[tokio::test]
async fn leases_fence_managers_and_recover_stale_birth() -> TestResult<()> {
    let scratch = Scratch::new()?;
    let launcher = scratch.supervisor()?;
    let first = scratch.manager(&launcher, 2)?;
    let second = scratch.manager(&launcher, 2)?;
    let original = check(first.start(FIRST, 0).await)?;
    assert!(Arc::ptr_eq(&original, &check(first.start(FIRST, 0).await)?));
    assert_eq!(
        second.start(FIRST, 0).await.err().map(|e| e.status),
        Some(409)
    );
    assert_ne!(
        original.display,
        check(second.start(SECOND, 0).await)?.display
    );
    assert_eq!(
        first.start(THIRD, 0).await.err().map(|e| e.status),
        Some(429)
    );
    check(first.stop(FIRST).await)?;
    check(second.stop(SECOND).await)?;
    let lease = Connection::open(scratch.0.join("leases/leases.sqlite"))?;
    lease.execute(
        "INSERT INTO leases VALUES(?,?,?,?,?,?)",
        params![
            original.display,
            first.root.to_string_lossy(),
            FIRST,
            "stale",
            std::process::id(),
            "wrong-birth",
        ],
    )?;
    assert_eq!(
        check(second.start(FIRST, 0).await)?.display,
        original.display
    );
    check(
        second
            .action(FIRST, "computer_click", &json!({"x":20,"y":30}), 0)
            .await,
    )?;
    assert_eq!(
        fs::read_to_string(scratch.0.join("launches"))?
            .lines()
            .count(),
        3
    );
    first.close().await;
    second.close().await;
    Ok(())
}

#[tokio::test]
async fn viewer_capture_does_not_extend_idle_lease_and_supervisor_is_reused() -> TestResult<()> {
    let scratch = Scratch::new()?;
    let launcher = scratch.supervisor()?;
    let manager = scratch.manager(&launcher, 1)?;
    let desktop = check(manager.start(FIRST, 0).await)?;
    for _ in 0..3 {
        let frame = check(manager.screenshot(FIRST, true).await)?;
        assert_eq!(image::guess_format(&frame)?, image::ImageFormat::Jpeg);
    }
    assert_eq!(check(manager.status(FIRST, 900))?["stopsInMs"], 100);
    for x in 0..8 {
        check(
            manager
                .action(FIRST, "computer_click", &json!({"x":x,"y":1}), 900)
                .await,
        )?;
    }
    check(
        manager
            .action(FIRST, "computer_screenshot", &json!({}), 900)
            .await,
    )?;
    assert_eq!(
        fs::read_to_string(scratch.0.join("launches"))?
            .lines()
            .count(),
        1
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("commands"))?
            .lines()
            .count(),
        12
    );
    check(manager.maintain(1500, |_| Ok(false)).await)?;
    assert!(check(manager.get(FIRST))?.is_some());
    let mut viewer = desktop.disconnected.subscribe();
    check(manager.maintain(1900, |_| Ok(false)).await)?;
    viewer.changed().await?;
    assert_eq!(check(manager.status(FIRST, 1900))?["state"], "off");
    let other = check(manager.start(SECOND, 2000).await)?;
    let mut viewer = other.disconnected.subscribe();
    fs::write(scratch.0.join("die"), [])?;
    let stopped = manager
        .action(SECOND, "computer_click", &json!({"x":1,"y":1}), 2000)
        .await
        .err()
        .ok_or("supervisor dies during a pending request")?;
    assert!(stopped.message.contains("helper stopped"));
    tokio::time::timeout(Duration::from_secs(3), async {
        while check(manager.get(SECOND))?.is_some() {
            check(manager.maintain(2001, |_| Ok(false)).await)?;
            tokio::task::yield_now().await;
        }
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await??;
    viewer.changed().await?;
    assert_eq!(check(manager.status(SECOND, 2001))?["state"], "off");
    let last = check(manager.start(THIRD, 2100).await)?;
    manager.close().await;
    assert!(last
        .helper
        .lock()
        .await
        .child
        .as_ref()
        .is_some_and(|child| child.id().is_none()));
    Ok(())
}

#[tokio::test]
async fn stopped_url_input_never_reaches_a_reused_display() -> TestResult<()> {
    let scratch = Scratch::new()?;
    let launcher = scratch.supervisor()?;
    let manager = scratch.manager(&launcher, 1)?;
    let first = check(manager.start(FIRST, 0).await)?;
    fs::write(scratch.0.join("pause"), [])?;
    let args = json!({"url":"https://example.test"});
    let action = manager.action(FIRST, "computer_open_url", &args, 1);
    let stop = async {
        scratch.observed("commands").await?;
        let release = async {
            while !first.stopped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            fs::write(scratch.0.join("release"), [])
        };
        let (stopped, released) = tokio::join!(manager.stop(FIRST), release);
        released?;
        check(stopped)
    };
    let (action, stopped) =
        tokio::time::timeout(Duration::from_secs(8), async { tokio::join!(action, stop) }).await?;
    stopped?;
    assert_eq!(
        action.err().ok_or("input stops with its desktop")?.status,
        409
    );
    assert_eq!(
        check(manager.start(SECOND, 1).await)?.display,
        first.display
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("commands"))?
            .lines()
            .count(),
        1
    );
    manager.close().await;
    Ok(())
}

#[tokio::test]
async fn slow_supervisor_holds_slot_until_exit() -> TestResult<()> {
    let scratch = Scratch::new()?;
    let launcher = scratch.supervisor()?;
    let manager = scratch.manager(&launcher, 1)?;
    check(manager.start(FIRST, 0).await)?;
    fs::write(scratch.0.join("slow"), [])?;
    let error = manager.stop(FIRST).await.err().ok_or("slow teardown")?;
    assert_eq!(error.status, 503);
    assert!(error.message.contains("lease remains reserved"));
    assert_eq!(
        manager.start(SECOND, 1).await.err().map(|e| e.status),
        Some(429)
    );
    fs::write(scratch.0.join("exited"), [])?;
    check(manager.stop(FIRST).await)?;
    check(manager.start(SECOND, 1).await)?;
    manager.close().await;
    Ok(())
}

#[tokio::test]
async fn project_environment_is_cleared_after_success_error_and_cancellation() -> TestResult<()> {
    let scratch = Scratch::new()?;
    let launcher = scratch.supervisor()?;
    let manager = scratch.manager(&launcher, 1)?;
    let env = HashMap::from([("PROJECT_TOKEN".into(), "project-canary".into())]);
    let click = json!({"x":1,"y":1});
    check(manager.action(FIRST, "computer_click", &click, 0).await)?;
    let (_active, cancelled) = tokio::sync::oneshot::channel();
    check(
        manager
            .action_with_environment(FIRST, "computer_click", &click, 0, &env, cancelled)
            .await,
    )?;
    assert_eq!(check(manager.status(FIRST, 0))?["state"], "off");
    fs::write(scratch.0.join("fail"), [])?;
    let (_active, cancelled) = tokio::sync::oneshot::channel();
    assert!(manager
        .action_with_environment(FIRST, "computer_click", &click, 0, &env, cancelled)
        .await
        .is_err());
    assert_eq!(check(manager.status(FIRST, 0))?["state"], "off");
    fs::remove_file(scratch.0.join("fail"))?;
    let commands = scratch.0.join("commands");
    let initial = fs::read_to_string(&commands)?;
    assert_eq!(
        initial
            .lines()
            .map(|line| line.split(' ').next().unwrap_or(""))
            .collect::<Vec<_>>(),
        ["unset", "project-canary", "project-canary"]
    );
    fs::remove_file(&commands)?;
    fs::write(scratch.0.join("pause"), [])?;
    let (active, cancelled) = tokio::sync::oneshot::channel();
    let cancel = async {
        scratch.observed("commands").await?;
        drop(active);
        fs::write(scratch.0.join("release"), [])?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    };
    let gate = check(manager.control_gate(FIRST))?;
    let order = gate.lock().await;
    let (action, cancelled, later) = tokio::join!(
        async {
            let _guard = order;
            manager
                .action_with_environment(FIRST, "computer_click", &click, 0, &env, cancelled)
                .await
        },
        cancel,
        async {
            let _guard = gate.lock().await;
            manager.action(FIRST, "computer_click", &click, 0).await
        },
    );
    assert_eq!(action.err().ok_or("cancelled action")?.status, 409);
    cancelled?;
    check(later)?;
    let logs = fs::read_to_string(commands)?;
    assert_eq!(
        logs.lines()
            .map(|line| line.split(' ').next().unwrap_or(""))
            .collect::<Vec<_>>(),
        ["project-canary", "unset"]
    );
    let launches = fs::read_to_string(scratch.0.join("launches"))?;
    assert_eq!(
        launches
            .lines()
            .filter(|line| line.ends_with(" project-canary"))
            .count(),
        3
    );
    assert!(launches
        .lines()
        .last()
        .is_some_and(|line| line.ends_with(" unset")));
    manager.close().await;
    Ok(())
}

fn viewer_state(scratch: &Scratch, manager: Manager) -> TestResult<crate::AppState> {
    let config = serde_json::from_value(json!({
        "database": scratch.0.join("state.sqlite"), "fixedClock": 0
    }))?;
    let config: Config = config;
    let database = crate::db::open(&config.database, false, 0)?;
    crate::files::init(&database.connection)?;
    crate::data::initialize(&database)?;
    crate::turns::initialize(&database.connection).map_err(|error| error.message)?;
    crate::computer::initialize(&database.connection)?;
    database.connection.execute(
        "INSERT INTO agents(id,name,model,instructions) VALUES(?,'Fixture','','')",
        [FIRST],
    )?;
    database
        .connection
        .execute("INSERT INTO agent_computers VALUES(?,1,1)", [FIRST])?;
    Ok(crate::AppState {
        database: Arc::new(Mutex::new(database)),
        config: Arc::new(config),
        origin: "http://127.0.0.1".into(),
        auth: None,
        events: crate::events::Hub::new(),
        computer: Arc::new(manager),
        turns: crate::turns::Runtime::default(),
        trial_pending: Arc::new(AtomicBool::new(false)),
    })
}

#[tokio::test]
async fn viewer_tickets_bind_agent_origin_expiry_and_stop() -> TestResult<()> {
    let scratch = Scratch::new()?;
    let launcher = scratch.supervisor()?;
    let state = viewer_state(&scratch, scratch.manager(&launcher, 1)?)?;
    let computer = &state.computer;
    let viewers = &computer.viewers;
    let mint = || {
        check(viewers.mint(
            &state,
            FIRST,
            &state.origin,
            &json!({"mode":"watch","revision":0}),
        ))
    };
    check(computer.start(FIRST, 0).await)?;
    for (agent, origin, time) in [
        (FIRST, state.origin.as_str(), 0),
        (SECOND, state.origin.as_str(), 0),
        (FIRST, "http://foreign.test", 0),
        (FIRST, state.origin.as_str(), 30_000),
    ] {
        let receipt = mint()?;
        assert_eq!(receipt["expiresAt"], 30_000);
        let token = receipt["ticket"].as_str().ok_or("Missing ticket")?;
        assert_eq!(
            viewers.consume(token, agent, origin, time).is_ok(),
            agent == FIRST && origin == state.origin && time == 0
        );
        assert!(viewers.consume(token, FIRST, &state.origin, 0).is_err());
    }
    let mut pending = Vec::new();
    for _ in 0..12 {
        pending.push(mint()?);
    }
    assert_eq!(
        viewers
            .mint(
                &state,
                FIRST,
                &state.origin,
                &json!({"mode":"watch","revision":0})
            )
            .err()
            .ok_or("ticket cap")?
            .status,
        429
    );
    check(computer.stop(FIRST).await)?;
    for receipt in pending {
        assert!(viewers
            .consume(
                receipt["ticket"].as_str().ok_or("Missing ticket")?,
                FIRST,
                &state.origin,
                0
            )
            .is_err());
    }
    computer.close().await;
    Ok(())
}

#[tokio::test]
async fn websocket_relay_checks_frames_and_revokes_viewers_on_idle_or_death() -> TestResult<()> {
    use axum::{extract::State, response::IntoResponse, routing::get, Router};
    use futures_util::StreamExt;
    use std::os::fd::AsRawFd;
    use tokio::{
        io::AsyncReadExt,
        net::{TcpListener, UnixListener},
    };
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

    fn masked(opcode: u8, data: &[u8]) -> Vec<u8> {
        let mask = [1, 2, 3, 4];
        let mut frame = vec![opcode, 128 | data.len() as u8];
        frame.extend(mask);
        frame.extend(
            data.iter()
                .enumerate()
                .map(|(index, byte)| byte ^ mask[index % 4]),
        );
        frame
    }

    let scratch = Scratch::new()?;
    let launcher = scratch.supervisor()?;
    let state = viewer_state(&scratch, scratch.manager(&launcher, 1)?)?;
    let computer = &state.computer;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    const RELAY_PATH: &str = "/relay";
    let router = Router::new()
        .route(
            RELAY_PATH,
            get(
                |State(state): State<crate::AppState>, request: axum::extract::Request| async move {
                    match super::super::vnc::upgrade(state, request, FIRST.into()).await {
                        Ok(response) => response,
                        Err(error) => (
                            axum::http::StatusCode::from_u16(error.status)
                                .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR),
                            error.message,
                        )
                            .into_response(),
                    }
                },
            ),
        )
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await });
    let mint = || -> TestResult<String> {
        let value = check(computer.viewers.mint(
            &state,
            FIRST,
            &state.origin,
            &json!({"mode":"watch","revision":0}),
        ))?;
        Ok(value["ticket"].as_str().ok_or("Missing ticket")?.into())
    };
    let connect = || async {
        let mut request =
            format!("ws://{address}{RELAY_PATH}?ticket={}", mint()?).into_client_request()?;
        request
            .headers_mut()
            .insert("origin", state.origin.parse()?);
        request
            .headers_mut()
            .insert("sec-websocket-protocol", "binary".parse()?);
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(
            tokio_tungstenite::connect_async(request).await?.0,
        )
    };
    for death in [false, true] {
        check(computer.start(FIRST, 0).await)?;
        let dir = fs::File::open(computer.root.join(FIRST).join("x11"))?;
        let backend = UnixListener::bind(format!("/proc/self/fd/{}/vnc.sock", dir.as_raw_fd()))?;
        if !death {
            for bytes in [
                masked(0x81, b"text"),
                vec![0x82, 0],
                vec![0x82, 0xff, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0],
            ] {
                let mut viewer = connect().await?;
                let (mut rfb, _) = backend.accept().await?;
                viewer.get_mut().write_all(&bytes).await?;
                let mut input = Vec::new();
                tokio::time::timeout(Duration::from_secs(3), rfb.read_to_end(&mut input)).await??;
                assert!(input.is_empty());
            }
        }
        let mut viewer = connect().await?;
        let (mut rfb, _) = backend.accept().await?;
        let bytes = [
            masked(0x02, b"RFB 003."),
            masked(0x80, b"008\n"),
            masked(0x89, b"hi"),
        ]
        .concat();
        for byte in bytes {
            viewer.get_mut().write_all(&[byte]).await?;
        }
        let mut input = [0u8; 12];
        tokio::time::timeout(Duration::from_secs(3), rfb.read_exact(&mut input)).await??;
        assert_eq!(&input, b"RFB 003.008\n");
        tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(message) = viewer.next().await {
                if matches!(message, Ok(Message::Pong(bytes)) if bytes == b"hi"[..]) {
                    return true;
                }
            }
            false
        })
        .await?
        .then_some(())
        .ok_or("No matching pong")?;
        let pending = mint()?;
        if death {
            let desktop = check(computer.get(FIRST))?.ok_or("Missing desktop")?;
            let mut helper = desktop.helper.lock().await;
            if let Some(child) = helper.child.as_mut() {
                child.kill().await?;
            }
        }
        check(
            computer
                .maintain(if death { 1 } else { 1000 }, |_| Ok(false))
                .await,
        )?;
        assert_eq!(check(computer.status(FIRST, 1000))?["state"], "off");
        assert!(computer
            .viewers
            .consume(&pending, FIRST, &state.origin, 0)
            .is_err());
        tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(message) = viewer.next().await {
                if let Ok(Message::Close(Some(frame))) = message {
                    assert_eq!(u16::from(frame.code), 4409);
                    return true;
                }
            }
            false
        })
        .await?
        .then_some(())
        .ok_or("Viewer did not close")?;
        let mut remaining = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), rfb.read_to_end(&mut remaining)).await??;
    }
    computer.close().await;
    server.abort();
    Ok(())
}

#[tokio::test]
async fn vault_provider_turns_fill_and_prompt_without_leaking() -> TestResult<()> {
    use axum::{routing::post, Json, Router};
    use std::sync::atomic::AtomicUsize;

    const LOGIN: &str = "vault-fixture-password-with-quotes-\"";
    const ASKED: &str = "vault-new-fixture-value";

    let scratch = Scratch::new()?;
    let launcher = scratch.script(
        r#"
echo ready
sequence=0
while IFS= read -r line; do
    sequence=$((sequence + 1))
    data=''
    case "$line" in
      *'"operation":"secret-target"'*) data='AQ==';;
      *'"operation":"secret-fill"'*'vault-fixture-password-with-quotes-'*) touch login-filled; data='AQ==';;
      *'"operation":"secret-fill"'*'vault-new-fixture-value'*) touch prompt-filled; data='AQ==';;
      *'"operation":"secret-fill"'*) data='AA==';;
      *'"operation":"capture"'*) touch capture-unexpected;;
    esac
    printf '{"id":%s,"data":"%s"}\n' "$sequence" "$data"
done
"#,
    )?;
    let mut state = viewer_state(&scratch, scratch.manager(&launcher, 2)?)?;
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let sequence = Arc::new(AtomicUsize::new(0));
    let router = Router::new().route(
        "/chat/completions",
        post({
            let requests = requests.clone();
            move |Json(request): Json<Value>| {
                let sequence = sequence.clone();
                let requests = requests.clone();
                async move {
                    if let Ok(mut requests) = requests.lock() {
                        requests.push(request);
                    }
                    let step = sequence.fetch_add(1, Ordering::SeqCst);
                    let (name, arguments) = match step {
                        0 | 5 => ("computer_fill_secret", json!({"entry":"fixture","which":"password"})),
                        1 => ("computer_screenshot", json!({})),
                        3 => ("computer_request_secret", json!({"entry":"missing","origin":"https://example.test"})),
                        _ => {
                            let echo = if step == 2 { LOGIN } else { ASKED };
                            return Json(json!({"choices":[{"message":{"content":format!("Fixture echo {echo}")},"finish_reason":"stop"}]}));
                        }
                    };
                    Json(json!({"choices":[{"message":{"tool_calls":[{
                        "id":format!("fixture-{step}"),"type":"function","function":{
                            "name":name,"arguments":arguments.to_string()
                        }
                    }]},"finish_reason":"tool_calls"}]}))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let config = Arc::get_mut(&mut state.config).ok_or("Own test configuration")?;
    config.data.base_url = Some(format!("http://{}", listener.local_addr()?));
    config.data.validate()?;
    config.remaining_data.test_keyring = Some(Default::default());
    state.turns = crate::turns::Runtime::new()?;
    let provider = tokio::spawn(async move { axum::serve(listener, router).await });

    let first = uuid::Uuid::new_v4().to_string();
    let second = uuid::Uuid::new_v4().to_string();
    let other = uuid::Uuid::new_v4().to_string();
    {
        let guard = state.database.lock().map_err(|_| "Test DB unavailable")?;
        let db = &guard.connection;
        crate::harness::initialize(db).map_err(|e| e.message)?;
        crate::automation::initialize(db).map_err(|e| e.message)?;
        crate::connectors::initialize(db).map_err(|e| e.message)?;
        crate::mcp::initialize(db).map_err(|e| e.message)?;
        db.execute(
            "INSERT INTO agents(id,name,model,instructions) VALUES(?,'Other','fixture','')",
            [SECOND],
        )?;
        db.execute("INSERT INTO agent_computers VALUES(?,1,1)", [SECOND])?;
        db.execute("UPDATE agents SET model='fixture'", [])?;
        for (id, agent) in [(&first, FIRST), (&second, FIRST), (&other, SECOND)] {
            db.execute(
                "INSERT INTO turns(id,agent,text,status,created) VALUES(?,?,'Sign in to the fixture.','running','2026-11-22T00:00:00Z')",
                [id.as_str(), agent],
            )?;
        }
    }
    let vault_request = |path: &str, body: Value, authorization: bool| -> TestResult<_> {
        let mut request = axum::extract::Request::builder()
            .uri(path)
            .method("POST")
            .header("host", "127.0.0.1")
            .header("origin", &state.origin)
            .header("content-type", "application/json");
        if authorization {
            request = request.header("authorization", "Bearer fixture-external");
        }
        Ok(crate::dispatch(
            axum::extract::State(state.clone()),
            request.body(axum::body::Body::from(body.to_string()))?,
        ))
    };
    let add = json!({
        "name":"fixture","kind":"login","revision":0,"url":"https://example.test/sign-in",
        "username":"vault-fixture-username","password":LOGIN,"notes":"vault-fixture-notes","agents":[FIRST]
    });
    assert_eq!(
        vault_request("/api/vault", add.clone(), true)?
            .await
            .status(),
        403
    );
    assert_eq!(vault_request("/api/vault", add, false)?.await.status(), 200);
    check(state.computer.start(FIRST, 0).await)?;
    check(crate::turns::execution::run(&state, &first).await)?;
    assert!(scratch.0.join("login-filled").exists());
    assert!(!scratch.0.join("capture-unexpected").exists());
    check(crate::turns::execution::run(&state, &second).await)?;
    {
        let guard = state.database.lock().map_err(|_| "Test DB unavailable")?;
        let wait = check(crate::computer::control::turn_wait(
            &guard.connection,
            &second,
        ))?
        .ok_or("Secure form did not pause")?;
        assert_eq!(wait["secret_entry"], "missing");
        assert_eq!(wait["secret_origin"], "https://example.test");
    }
    let answer = json!({"turn":second,"agent":FIRST,"call":"fixture-3","value":ASKED});
    assert_eq!(
        vault_request("/api/vault/respond", answer.clone(), false)?
            .await
            .status(),
        200
    );
    assert!(scratch.0.join("prompt-filled").exists());
    assert_eq!(
        vault_request("/api/vault/respond", answer, false)?
            .await
            .status(),
        409
    );
    check(crate::turns::execution::run(&state, &second).await)?;
    check(crate::turns::execution::run(&state, &other).await)?;
    state.computer.close().await;
    provider.abort();

    let requests = requests.lock().map_err(|_| "Test recorder unavailable")?;
    assert_eq!(requests.len(), 7);
    for request in requests.iter() {
        assert!(!request
            .to_string()
            .contains("vault-fixture-password-with-quotes-"));
        assert!(!request.to_string().contains(ASKED));
    }
    assert!(requests[1]
        .to_string()
        .contains("{{vault:fixture.password}}"));
    assert!(requests[2].to_string().contains("Screenshots are hidden"));
    assert!(requests[4].to_string().contains("{{vault:missing.value}}"));
    assert!(requests[6]
        .to_string()
        .contains("Vault entry unavailable to this agent."));

    let db = state.database.lock().map_err(|_| "Test DB unavailable")?;
    let db = &db.connection;
    for id in [&first, &second, &other] {
        let stored: (String, String) =
            db.query_row("SELECT status,reply FROM turns WHERE id=?", [id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
        assert_eq!(stored.0, "done");
        assert!(!stored.1.contains("vault-fixture-password-with-quotes-"));
        assert!(!stored.1.contains(ASKED));
    }
    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    let bytes = fs::read(&state.config.database)?;
    for secret in [
        "vault-fixture-password-with-quotes-",
        "vault-fixture-username",
        "vault-fixture-notes",
        ASKED,
    ] {
        assert!(!bytes
            .windows(secret.len())
            .any(|field| field == secret.as_bytes()));
    }
    Ok(())
}

#[tokio::test]
async fn shared_browser_profile_preserves_state_and_has_one_writer() -> TestResult<()> {
    let scratch = Scratch::new()?;
    let launcher = scratch.supervisor()?;
    let manager = scratch.manager(&launcher, 3)?;
    check(manager.set_shared_profile(FIRST, true))?;
    check(manager.set_shared_profile(SECOND, true))?;
    check(manager.start(FIRST, 0).await)?;
    assert_eq!(
        manager.start(SECOND, 0).await.err().map(|e| e.status),
        Some(409)
    );
    check(manager.start(THIRD, 0).await)?;
    let shared = manager.root.join("owner-chrome");
    fs::write(shared.join("Default/session-fixture"), "session")?;
    check(manager.stop(FIRST).await)?;
    check(manager.start(SECOND, 0).await)?;
    assert_eq!(
        fs::read_to_string(shared.join("Default/session-fixture"))?,
        "session"
    );
    assert!(!manager
        .root
        .join(THIRD)
        .join("chrome/Default/session-fixture")
        .exists());
    manager.close().await;
    Ok(())
}
