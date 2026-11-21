use super::desktop_secret::ChromePipe;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{self, BufRead, Read, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    net::{TcpListener, UnixStream},
    process::{Child, Command},
    signal::unix::{signal, SignalKind},
    sync::{mpsc, oneshot},
    time::{interval, sleep, timeout, Instant},
};

type Result<T> = crate::db::Result<T>;
pub(super) const BINARY: &str = "/minarbot-server";
pub(super) const PROXY: &str = "/run/minarbot/proxy.sock";
const HOME: &str = "/home/desktop";
const PROXY_URL: &str = "http://127.0.0.1:3128";
const MAX_OUTPUT: u64 = 1024 * 1024;
const START_TIMEOUT: Duration = Duration::from_secs(20);

fn seed_desktop() -> io::Result<()> {
    let base = Path::new(HOME).join(".config/xfce4");
    let channels = base.join("xfconf/xfce-perchannel-xml");
    std::fs::create_dir_all(&channels)?;
    let launcher = base.join("panel/launcher-1");
    std::fs::create_dir_all(&launcher)?;
    let panel = channels.join("xfce4-panel.xml");
    if panel.exists() && !std::fs::read_to_string(&panel)?.contains("applicationsmenu") {
        std::fs::remove_file(panel)?;
    }
    for (name, xml) in [
        (
            "xfwm4",
            r#"<property name="general" type="empty"><property name="use_compositing" type="bool" value="false"/><property name="workspace_count" type="int" value="1"/></property>"#,
        ),
        (
            "xfce4-desktop",
            r#"<property name="backdrop" type="empty"><property name="screen0" type="empty"><property name="monitorVNC-0" type="empty"><property name="workspace0" type="empty"><property name="image-style" type="int" value="0"/><property name="color-style" type="int" value="0"/><property name="rgba1" type="array"><value type="double" value="0.11"/><value type="double" value="0.12"/><value type="double" value="0.16"/><value type="double" value="1"/></property></property></property></property></property><property name="desktop-icons" type="empty"><property name="style" type="int" value="0"/></property>"#,
        ),
        (
            "xfce4-panel",
            r#"<property name="configver" type="int" value="2"/><property name="panels" type="array"><value type="int" value="1"/><property name="panel-1" type="empty"><property name="position" type="string" value="p=6;x=0;y=0"/><property name="position-locked" type="bool" value="true"/><property name="length" type="uint" value="100"/><property name="size" type="uint" value="32"/><property name="plugin-ids" type="array"><value type="int" value="1"/><value type="int" value="5"/><value type="int" value="2"/><value type="int" value="3"/><value type="int" value="4"/></property></property></property><property name="plugins" type="empty"><property name="plugin-5" type="string" value="applicationsmenu"><property name="button-title" type="string" value="Apps"/><property name="show-button-title" type="bool" value="true"/><property name="show-generic-names" type="bool" value="false"/></property><property name="plugin-1" type="string" value="launcher"><property name="items" type="array"><value type="string" value="minarbot-terminal.desktop"/></property></property><property name="plugin-2" type="string" value="tasklist"/><property name="plugin-3" type="string" value="separator"><property name="expand" type="bool" value="true"/><property name="style" type="uint" value="0"/></property><property name="plugin-4" type="string" value="clock"/></property>"#,
        ),
    ] {
        let file = channels.join(format!("{name}.xml"));
        if !file.exists() {
            std::fs::write(
                file,
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?><channel name="{name}" version="1.0">{xml}</channel>"#
                ),
            )?;
        }
    }
    std::fs::write(
        launcher.join("minarbot-terminal.desktop"),
        "[Desktop Entry]\nType=Application\nName=Terminal\nIcon=utilities-terminal\nExec=xfce4-terminal --disable-server\nTerminal=false\n",
    )
}

struct Program {
    name: &'static str,
    child: Child,
    critical: bool,
}

struct Supervisor {
    environment: BTreeMap<OsString, OsString>,
    programs: Vec<Program>,
    chrome: Option<ChromePipe>,
}

impl Supervisor {
    fn new() -> Result<Self> {
        let display = std::env::var("DISPLAY")?;
        let display = display.strip_prefix("unix/").unwrap_or(&display);
        if !display
            .strip_prefix(':')
            .and_then(|value| value.parse::<u16>().ok())
            .is_some_and(|value| (100..300).contains(&value))
        {
            return Err("Invalid desktop display.".into());
        }
        let mut environment = std::env::vars_os().collect::<BTreeMap<_, _>>();
        for (name, value) in [
            ("DISPLAY", display),
            ("http_proxy", PROXY_URL),
            ("https_proxy", PROXY_URL),
            ("HTTP_PROXY", PROXY_URL),
            ("HTTPS_PROXY", PROXY_URL),
            ("ALL_PROXY", PROXY_URL),
            ("no_proxy", ""),
            ("NO_PROXY", ""),
            ("XDG_CURRENT_DESKTOP", "XFCE"),
            ("XDG_SESSION_TYPE", "x11"),
        ] {
            environment.insert(name.into(), value.into());
        }
        Ok(Self {
            environment,
            programs: Vec::new(),
            chrome: None,
        })
    }

    fn command(&self, name: &str) -> Command {
        let mut command = Command::new(name);
        command
            .env_clear()
            .envs(&self.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command.as_std_mut().process_group(0);
        command
    }

    fn launch(&mut self, name: &'static str, arguments: &[&str], critical: bool) -> Result<()> {
        let child = self.command(name).args(arguments).spawn()?;
        self.programs.push(Program {
            name,
            child,
            critical,
        });
        Ok(())
    }

    fn check(&mut self) -> Result<()> {
        for program in &mut self.programs {
            if program.critical && program.child.try_wait()?.is_some() {
                return Err(format!("Desktop {} stopped.", program.name).into());
            }
        }
        Ok(())
    }

    async fn tool<S: AsRef<std::ffi::OsStr>>(
        &self,
        name: &str,
        arguments: &[S],
        deadline: Duration,
    ) -> Result<Vec<u8>> {
        let mut command = self.command(name);
        command.args(arguments).stdout(Stdio::piped());
        let mut child = command.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or("Desktop tool output unavailable.")?;
        let result = timeout(deadline, async {
            let mut bytes = Vec::new();
            stdout.take(MAX_OUTPUT + 1).read_to_end(&mut bytes).await?;
            if bytes.len() as u64 > MAX_OUTPUT {
                return Err("Desktop tool output exceeded its limit.".into());
            }
            if !child.wait().await?.success() {
                return Err("Desktop tool failed.".into());
            }
            Ok(bytes)
        })
        .await;
        if result.as_ref().is_err() || result.as_ref().is_ok_and(|result| result.is_err()) {
            signal_group(&child, rustix::process::Signal::KILL);
        }
        result?
    }

    async fn wait_for(&mut self, arguments: &[&str], watch_chrome: bool) -> Result<()> {
        let deadline = Instant::now() + START_TIMEOUT;
        while Instant::now() < deadline {
            self.check()?;
            if watch_chrome
                && self.programs.last_mut().is_some_and(|program| {
                    program.child.try_wait().is_ok_and(|child| child.is_some())
                })
            {
                break;
            }
            if self
                .tool("xdotool", arguments, Duration::from_secs(2))
                .await
                .is_ok()
            {
                return Ok(());
            }
            sleep(Duration::from_millis(100)).await;
        }
        Err("Desktop did not become ready.".into())
    }

    async fn start_bus(&mut self) -> Result<()> {
        let mut child = self
            .command("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or("Private D-Bus output unavailable.")?;
        self.programs.push(Program {
            name: "dbus-daemon",
            child,
            critical: false,
        });
        let address = timeout(
            Duration::from_secs(5),
            BufReader::new(stdout).lines().next_line(),
        )
        .await??
        .filter(|text| !text.is_empty())
        .ok_or("Private D-Bus did not start.")?;
        self.environment
            .insert("DBUS_SESSION_BUS_ADDRESS".into(), address.into());
        Ok(())
    }

    fn launch_chrome(&mut self, no_sandbox: bool) -> Result<Arc<Mutex<Vec<u8>>>> {
        let mut command = self.command("sh");
        let pipe = ChromePipe::configure(&mut command)?;
        command.args([
            "--user-data-dir=/home/desktop/chrome",
            "--no-first-run",
            "--no-default-browser-check",
            "--hide-crash-restore-bubble",
            "--disable-sync",
            "--disable-extensions",
            "--disable-background-networking",
            "--disable-features=Signin,ChromeWhatsNewUI,MediaRouter",
            "--password-store=basic",
            "--window-size=1280,800",
            "--start-maximized",
            "--proxy-server=http://127.0.0.1:3128",
            "--proxy-bypass-list=<-loopback>",
            "--disable-quic",
            "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
        ]);
        if no_sandbox {
            command.arg("--no-sandbox");
        }
        let mut child = command.arg("about:blank").stderr(Stdio::piped()).spawn()?;
        self.chrome = Some(pipe);
        let mut stderr = child
            .stderr
            .take()
            .ok_or("Chrome diagnostics unavailable.")?;
        self.programs.push(Program {
            name: "Chrome",
            child,
            critical: false,
        });
        let diagnostics = Arc::new(Mutex::new(Vec::<u8>::new()));
        let captured = diagnostics.clone();
        tokio::spawn(async move {
            let mut chunk = [0u8; 4096];
            while let Ok(size) = stderr.read(&mut chunk).await {
                if size == 0 {
                    break;
                }
                let Ok(mut bytes) = captured.lock() else {
                    break;
                };
                bytes.extend_from_slice(&chunk[..size]);
                let excess = bytes.len().saturating_sub(16_384);
                bytes.drain(..excess);
            }
        });
        Ok(diagnostics)
    }

    async fn start(&mut self) -> Result<()> {
        seed_desktop()?;
        self.start_bus().await?;
        let display = self
            .environment
            .get(std::ffi::OsStr::new("DISPLAY"))
            .and_then(|value| value.to_str())
            .ok_or("Desktop display unavailable.")?
            .to_owned();
        let geometry =
            std::env::var("MINARBOT_DESKTOP_GEOMETRY").unwrap_or_else(|_| "1280x800".into());
        super::options::dimensions(&geometry)?;
        self.launch(
            "Xvnc",
            &[
                &display,
                "-geometry",
                &geometry,
                "-depth",
                "24",
                "-dpi",
                "96",
                "-rfbport",
                "-1",
                "-rfbunixpath",
                "/run/minarbot/vnc.sock",
                "-rfbunixmode",
                "0600",
                "-SecurityTypes",
                "None",
                "-AlwaysShared",
                "-AcceptSetDesktopSize",
                "-FrameRate",
                "30",
                "-MaxCutText",
                "262144",
                "-desktop",
                "Minarbot desktop",
                "-nolisten",
                "tcp",
            ],
            true,
        )?;
        self.wait_for(&["getdisplaygeometry"], false).await?;
        self.launch("xfwm4", &["--compositor=off", "--sm-client-disable"], true)?;
        sleep(Duration::from_millis(150)).await;
        self.launch(
            "xfdesktop",
            &["--sm-client-disable", "--disable-wm-check"],
            true,
        )?;
        self.launch(
            "xfce4-panel",
            &["--sm-client-disable", "--disable-wm-check"],
            true,
        )?;
        let search = ["search", "--onlyvisible", "--class", "google-chrome"];
        let diagnostics = self.launch_chrome(false)?;
        if let Err(error) = self.wait_for(&search, true).await {
            self.check()?;
            let sandbox_failure = diagnostics.lock().is_ok_and(|bytes| {
                let message = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
                message.contains("failed to move to new namespace")
                    || message.contains("no usable sandbox")
                    || message.lines().any(|line| {
                        line.contains("zygote_host_impl_linux")
                            && line.contains("operation not permitted")
                    })
            });
            if !sandbox_failure {
                return Err(error);
            }
            if let Some(previous) = self.programs.last_mut() {
                signal_group(&previous.child, rustix::process::Signal::KILL);
                let _ = previous.child.wait().await;
            }
            self.launch_chrome(true)?;
            self.wait_for(&search, true).await?;
            writeln!(io::stdout(), "chrome-no-sandbox")?;
        }
        self.wait_for(
            &[
                "search",
                "--onlyvisible",
                "--class",
                "google-chrome",
                "windowactivate",
                "--sync",
            ],
            true,
        )
        .await?;
        if let Some(chrome) = self.programs.last_mut() {
            chrome.critical = true;
        }
        Ok(())
    }

    async fn action(&self, request: &Request) -> Result<Vec<u8>> {
        let deadline = Duration::from_secs(10);
        match request.operation.as_str() {
            "input" => self.tool("xdotool", &request.args, deadline).await,
            "secret-target" | "secret-fill" => {
                let [origin, target, fields @ ..] = request.args.as_slice() else {
                    return Err("Invalid secure input.".into());
                };
                let value = match (request.operation.as_str(), fields) {
                    ("secret-target", []) => None,
                    ("secret-fill", [value]) => Some(value.as_str()),
                    _ => return Err("Invalid secure input.".into()),
                };
                let chrome = self.chrome.as_ref().ok_or("Browser is unavailable.")?;
                let matched =
                    tokio::time::timeout(deadline, chrome.focused(origin, target, value)).await??;
                Ok(vec![u8::from(matched)])
            }
            "capture" => {
                let file = format!("/tmp/minarbot-{}.png", request.id);
                let result = async {
                    self.tool("scrot", &["-z", &file], deadline).await?;
                    let bytes = tokio::fs::File::open(&file).await?;
                    let mut image = Vec::new();
                    bytes
                        .take(16 * 1024 * 1024 + 1)
                        .read_to_end(&mut image)
                        .await?;
                    if image.len() > 16 * 1024 * 1024 {
                        return Err("Desktop capture exceeds its limit.".into());
                    }
                    Ok(image)
                }
                .await;
                let _ = tokio::fs::remove_file(&file).await;
                result
            }
            _ => Err("Unknown desktop helper command.".into()),
        }
    }

    async fn stop(&mut self) {
        for program in self.programs.iter().rev() {
            signal_group(&program.child, rustix::process::Signal::TERM);
        }
        sleep(Duration::from_secs(1)).await;
        for program in self.programs.iter().rev() {
            signal_group(&program.child, rustix::process::Signal::KILL);
        }
    }
}

fn signal_group(child: &Child, signal: rustix::process::Signal) {
    if let Some(pid) = child
        .id()
        .and_then(|pid| rustix::process::Pid::from_raw(pid as i32))
    {
        let _ = rustix::process::kill_process_group(pid, signal);
    }
}

#[derive(Deserialize)]
struct Request {
    id: u64,
    operation: String,
    args: Vec<String>,
}

async fn forward(listener: TcpListener) {
    while let Ok((mut client, _)) = listener.accept().await {
        tokio::spawn(async move {
            if let Ok(mut upstream) = UnixStream::connect(PROXY).await {
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            }
        });
    }
}

pub(crate) async fn run() -> Result<()> {
    if std::env::current_exe()?.as_os_str() != BINARY
        || std::env::args_os().nth(2).as_deref() != Some(std::ffi::OsStr::new(PROXY))
        || std::env::args_os().len() != 3
    {
        return Err("The desktop supervisor requires its sandbox and filtering proxy.".into());
    }
    let listener = TcpListener::bind(("127.0.0.1", 3128)).await?;
    let proxy = tokio::spawn(forward(listener));
    let mut supervisor = Supervisor::new()?;
    let mut term = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let (send, mut requests) = mpsc::channel(1);
    let (finished, mut eof) = oneshot::channel();
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = Read::take(&mut stdin, 65_537).read_until(b'\n', &mut line);
            if !read.is_ok_and(|size| (1..=65_536).contains(&size)) {
                break;
            }
            if let Ok(request) = serde_json::from_slice::<Request>(&line) {
                if send.blocking_send(request).is_err() {
                    break;
                }
            }
        }
        let _ = finished.send(());
    });
    let ready = tokio::select! {
        result = supervisor.start() => Some(result),
        _ = term.recv() => None,
        _ = interrupt.recv() => None,
        _ = &mut eof => None,
    };
    let result = match ready {
        Some(Ok(())) => {
            async {
                writeln!(io::stdout(), "ready")?;
                let mut monitor = interval(Duration::from_millis(100));
                loop {
                    let request = tokio::select! {
                        request = requests.recv() => request,
                        _ = monitor.tick() => { supervisor.check()?; continue; }
                        _ = term.recv() => None,
                        _ = interrupt.recv() => None,
                        _ = &mut eof => None,
                    };
                    let Some(request) = request else {
                        break;
                    };
                    let result = tokio::select! {
                        result = supervisor.action(&request) => result,
                        _ = term.recv() => break,
                        _ = interrupt.recv() => break,
                        _ = &mut eof => break,
                    };
                    let message = match result {
                        Ok(data) => json!({"id":request.id,"data":STANDARD.encode(data)}),
                        Err(_) => json!({"id":request.id,"error":true}),
                    };
                    writeln!(io::stdout(), "{message}")?;
                }
                Ok(())
            }
            .await
        }
        Some(Err(error)) => {
            let _ = writeln!(io::stdout(), "failed");
            Err(error)
        }
        None => Ok(()),
    };
    proxy.abort();
    supervisor.stop().await;
    result
}
