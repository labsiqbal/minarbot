//! Headless Chrome for UI smokes, driven over the DevTools protocol.
//!
//! [`smoke`] launches one browser tab, runs the journey, and fails it when
//! the page logged a console error, threw, or saw a failed or 4xx/5xx
//! request. On failure it saves a screenshot and adds the URL, the
//! problems and the visible text to the error.
//!
//! The browser is `MINARBOT_CHROME`, else the first of `google-chrome`,
//! `google-chrome-stable`, `chromium` and `chromium-browser` on `PATH`. A
//! missing browser fails the smoke; `MINARBOT_SKIP_BROWSER=1` skips it.
//! Screenshots go to `MINARBOT_SMOKE_ARTIFACTS`, else
//! `target/<profile>/smoke-artifacts`.
//!
//! Chrome talks CDP over `--remote-debugging-pipe` (the transport the
//! server's Computer browser uses), so no debugging port is open on the
//! host. Elements are found by role and accessible name through Chrome's
//! own accessibility tree (`Accessibility.queryAXTree`), and clicks are real
//! mouse events at the element's center.

use crate::{fakes, server::WAIT, wait, Error, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use rustix::process::{kill_process_group, Pid, Signal};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::File,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{unix::OwnedWriteHalf, UnixStream},
    process::{Child, Command},
    sync::oneshot,
    task::JoinHandle,
};

const BROWSERS: &[&str] = &[
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
];
const VIEWPORT: (u32, u32) = (1280, 900);
/// Visible text included in a failure message.
const TEXT_EXCERPT: usize = 2000;

/// Built Leptos client (`app/dist`), for the server's `dist` option.
/// Fails with the build command when `cargo xtask leptos` has not run.
pub fn client_dist() -> Result<PathBuf> {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/dist");
    if dist.join("_leptos/conversation.html").is_file() {
        Ok(dist)
    } else {
        Err(format!(
            "{} has no Leptos client: run `cargo xtask leptos` before the UI smokes",
            dist.display()
        )
        .into())
    }
}

/// Run `journey` in a fresh browser tab named `name` (used for the
/// screenshot file). The smoke fails on a journey error and on any page
/// problem left at the end; see [`Browser::problems`].
pub async fn smoke(name: &str, journey: impl AsyncFnOnce(&Browser) -> Result<()>) -> Result<()> {
    if std::env::var_os("MINARBOT_SKIP_BROWSER").is_some_and(|value| value == "1") {
        eprintln!("skipped UI smoke {name}: MINARBOT_SKIP_BROWSER=1");
        return Ok(());
    }
    let browser = Browser::launch().await?;
    let mut outcome = journey(&browser).await;
    if outcome.is_ok() {
        let problems = browser.problems();
        if !problems.is_empty() {
            outcome = Err(format!("page problems:\n  {}", problems.join("\n  ")).into());
        }
    }
    match outcome {
        Ok(()) => Ok(()),
        Err(error) => {
            // libtest prints a returned error with `Debug`, which escapes
            // newlines, so the multi-line report goes to the captured stderr.
            eprintln!("{}", browser.report(name, &error).await);
            Err(format!("UI smoke {name} failed: {error}").into())
        }
    }
}

/// One headless Chrome with one tab. Dropping it kills the browser's
/// process group and removes its profile.
pub struct Browser {
    child: Child,
    profile: PathBuf,
    cdp: Cdp,
    session: String,
    state: Arc<Mutex<PageState>>,
}

/// A located element: a DOM node by backend id, valid until it is removed.
#[derive(Clone, Copy, Debug)]
pub struct Element {
    node: u64,
}

#[derive(Default)]
struct PageState {
    problems: Vec<String>,
    loads: u64,
    requests: HashMap<String, String>,
}

impl Browser {
    /// Launch the browser (see the module docs for discovery).
    pub async fn launch() -> Result<Self> {
        let binary = find_browser()?;
        let profile = std::env::temp_dir().join(format!("minarbot-browser-{}", crate::new_id()));
        std::fs::create_dir_all(&profile)?;
        let log = File::create(profile.join("chrome.log"))?;
        let (ours, theirs) = std::os::unix::net::UnixStream::pair()?;
        ours.set_nonblocking(true)?;
        let input: std::os::fd::OwnedFd = theirs.try_clone()?.into();
        let output: std::os::fd::OwnedFd = theirs.into();
        let (width, height) = VIEWPORT;
        // Chrome reads CDP on fd 3 and writes on fd 4.
        let child = Command::new("/bin/sh")
            .args([
                "-c",
                r#"exec 3<&0 4>&1 </dev/null >/dev/null; exec "$@""#,
                "minarbot-smoke-chrome",
            ])
            .arg(&binary)
            .args([
                "--headless",
                "--remote-debugging-pipe",
                // Without a keyring, Chrome's first request waits ~25 s for
                // the D-Bus secret service.
                "--password-store=basic",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-gpu",
                "--disable-dev-shm-usage",
                "--disable-background-networking",
                "--disable-extensions",
                "--disable-sync",
                "--mute-audio",
                "--hide-scrollbars",
                &format!("--window-size={width},{height}"),
            ])
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg("about:blank")
            .stdin(Stdio::from(input))
            .stdout(Stdio::from(output))
            .stderr(Stdio::from(log))
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("{} did not start: {error}", binary.display()))?;
        let state = Arc::new(Mutex::new(PageState::default()));
        let cdp = Cdp::start(UnixStream::from_std(ours)?, state.clone());
        let mut browser = Self {
            child,
            profile,
            cdp,
            session: String::new(),
            state,
        };
        browser
            .attach()
            .await
            .map_err(|error| browser.crashed(error))?;
        Ok(browser)
    }

    async fn attach(&mut self) -> Result<()> {
        let created = self
            .cdp
            .call(None, "Target.createTarget", json!({"url": "about:blank"}))
            .await?;
        let target = created["targetId"].as_str().ok_or("no tab")?.to_owned();
        let attached = self
            .cdp
            .call(
                None,
                "Target.attachToTarget",
                json!({"targetId": target, "flatten": true}),
            )
            .await?;
        self.session = attached["sessionId"]
            .as_str()
            .ok_or("no CDP session")?
            .to_owned();
        for domain in ["Page", "Runtime", "Network", "Log"] {
            self.call(&format!("{domain}.enable"), json!({})).await?;
        }
        Ok(())
    }

    fn crashed(&self, error: Error) -> Error {
        let log = std::fs::read_to_string(self.profile.join("chrome.log")).unwrap_or_default();
        let tail: Vec<&str> = log.lines().rev().take(20).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        format!("browser failed: {error}\nchrome.log:\n{}", tail.join("\n")).into()
    }

    /// One CDP command on the tab's session.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.cdp.call(Some(&self.session), method, params).await
    }

    /// Navigate and wait for the load event.
    pub async fn goto(&self, url: &str) -> Result<()> {
        let before = self.loads();
        let navigation = self.call("Page.navigate", json!({"url": url})).await?;
        if let Some(error) = navigation["errorText"].as_str() {
            return Err(format!("navigation to {url} failed: {error}").into());
        }
        self.wait_load(before, url).await
    }

    /// Reload the page and wait for the load event.
    pub async fn reload(&self) -> Result<()> {
        let before = self.loads();
        self.call("Page.reload", json!({})).await?;
        self.wait_load(before, "reload").await
    }

    fn loads(&self) -> u64 {
        self.state
            .lock()
            .map(|state| state.loads)
            .unwrap_or_default()
    }

    async fn wait_load(&self, before: u64, label: &str) -> Result<()> {
        wait::until(format!("load event of {label}"), || async {
            Ok((self.loads() > before).then_some(()))
        })
        .await
    }

    /// Evaluate `expression` (promises are awaited) and return its JSON value.
    pub async fn evaluate(&self, expression: &str) -> Result<Value> {
        let reply = self
            .call(
                "Runtime.evaluate",
                json!({"expression": expression, "returnByValue": true, "awaitPromise": true}),
            )
            .await?;
        if let Some(details) = reply.get("exceptionDetails") {
            return Err(format!("evaluate {expression}: {}", exception_text(details)).into());
        }
        Ok(reply["result"]["value"].clone())
    }

    /// Current URL.
    pub async fn url(&self) -> Result<String> {
        Ok(self
            .evaluate("location.href")
            .await?
            .as_str()
            .unwrap_or_default()
            .to_owned())
    }

    /// Visible text of the page (`document.body.innerText`).
    pub async fn text(&self) -> Result<String> {
        Ok(
            settled(|| self.evaluate("document.body ? document.body.innerText : ''"))
                .await?
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        )
    }

    /// Wait until the visible text contains `text`.
    pub async fn wait_text(&self, text: &str) -> Result<()> {
        wait::until(format!("text {text:?}"), || async {
            Ok(self.text().await?.contains(text).then_some(()))
        })
        .await
    }

    /// Wait until the visible text no longer contains `text`.
    pub async fn wait_no_text(&self, text: &str) -> Result<()> {
        wait::until(format!("no text {text:?}"), || async {
            Ok((!self.text().await?.contains(text)).then_some(()))
        })
        .await
    }

    /// Elements with `role` (and, if given, the exact accessible name
    /// `name`) in the accessibility tree, optionally inside `within`, with
    /// their accessible names.
    pub async fn query(
        &self,
        role: &str,
        name: Option<&str>,
        within: Option<Element>,
    ) -> Result<Vec<(Element, String)>> {
        settled(|| self.query_once(role, name, within)).await
    }

    async fn query_once(
        &self,
        role: &str,
        name: Option<&str>,
        within: Option<Element>,
    ) -> Result<Vec<(Element, String)>> {
        let mut params = match within {
            Some(element) => json!({"backendNodeId": element.node}),
            None => {
                let document = self
                    .call("Runtime.evaluate", json!({"expression": "document"}))
                    .await?;
                let object = document["result"]["objectId"]
                    .as_str()
                    .ok_or("document has no object id")?;
                json!({"objectId": object})
            }
        };
        params["role"] = json!(role);
        if let Some(name) = name {
            params["accessibleName"] = json!(name);
        }
        let found = self.call("Accessibility.queryAXTree", params).await?;
        Ok(found["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|node| node["ignored"] != true)
            .filter_map(|node| {
                let element = Element {
                    node: node["backendDOMNodeId"].as_u64()?,
                };
                let name = node["name"]["value"].as_str().unwrap_or_default();
                Some((element, name.to_owned()))
            })
            .collect())
    }

    /// Wait for exactly one element with `role` and accessible name `name`.
    pub async fn find(&self, role: &str, name: &str) -> Result<Element> {
        self.find_in(role, name, None).await
    }

    /// [`Browser::find`] inside `within` (for example a dialog).
    pub async fn find_in(
        &self,
        role: &str,
        name: &str,
        within: Option<Element>,
    ) -> Result<Element> {
        let count = &std::sync::atomic::AtomicUsize::new(0);
        let found = wait::until(format!("one {role} named {name:?}"), || async move {
            let found = self.query(role, Some(name), within).await?;
            count.store(found.len(), std::sync::atomic::Ordering::Relaxed);
            Ok(match found.as_slice() {
                [(element, _)] => Some(*element),
                _ => None,
            })
        })
        .await;
        found.map_err(|error| {
            let count = count.load(std::sync::atomic::Ordering::Relaxed);
            format!("{error} (last match count {count})").into()
        })
    }

    /// Wait until no element with `role` and `name` remains.
    pub async fn gone(&self, role: &str, name: &str) -> Result<()> {
        wait::until(format!("no {role} named {name:?}"), || async {
            Ok(self
                .query(role, Some(name), None)
                .await?
                .is_empty()
                .then_some(()))
        })
        .await
    }

    /// Find, then click with a real mouse press at the element's center.
    pub async fn click(&self, role: &str, name: &str) -> Result<()> {
        let element = self.find(role, name).await?;
        self.click_element(element).await
    }

    /// Click `element` at its center.
    pub async fn click_element(&self, element: Element) -> Result<()> {
        let node = json!({"backendNodeId": element.node});
        self.call("DOM.scrollIntoViewIfNeeded", node.clone())
            .await?;
        let quads = self.call("DOM.getContentQuads", node).await?;
        let quad = quads["quads"][0]
            .as_array()
            .filter(|quad| quad.len() == 8)
            .ok_or("element has no box to click")?;
        // Four (x, y) corners; click their mean.
        let coordinate = |offset: usize| {
            quad.iter()
                .skip(offset)
                .step_by(2)
                .filter_map(Value::as_f64)
                .sum::<f64>()
                / 4.0
        };
        let (x, y) = (coordinate(0), coordinate(1));
        for (kind, buttons) in [("mouseMoved", 0), ("mousePressed", 1), ("mouseReleased", 0)] {
            self.call(
                "Input.dispatchMouseEvent",
                json!({"type": kind, "x": x, "y": y, "button": "left", "buttons": buttons, "clickCount": 1}),
            )
            .await?;
        }
        Ok(())
    }

    /// Replace the value of the text field `name` by typing `text`.
    pub async fn fill(&self, role: &str, name: &str, text: &str) -> Result<()> {
        let element = self.find(role, name).await?;
        self.fill_element(element, text).await
    }

    /// Focus `element`, select its value and type `text` over it.
    pub async fn fill_element(&self, element: Element, text: &str) -> Result<()> {
        self.call("DOM.focus", json!({"backendNodeId": element.node}))
            .await?;
        self.on(
            element,
            "function() { this.select ? this.select() : document.execCommand('selectAll'); }",
        )
        .await?;
        if text.is_empty() {
            self.key("Backspace").await
        } else {
            self.call("Input.insertText", json!({"text": text})).await?;
            Ok(())
        }
    }

    /// Choose the option of the `<select>` `name` whose value or label is `option`.
    pub async fn select(&self, name: &str, option: &str) -> Result<()> {
        let element = self.find("combobox", name).await?;
        let chosen = self
            .on(
                element,
                &format!(
                    "function() {{ const want = {}; \
                     const option = [...this.options].find(o => o.value === want || o.label === want); \
                     if (!option) return [...this.options].map(o => o.label); \
                     this.value = option.value; \
                     this.dispatchEvent(new Event('input', {{bubbles: true}})); \
                     this.dispatchEvent(new Event('change', {{bubbles: true}})); return true; }}",
                    json!(option)
                ),
            )
            .await?;
        if chosen == true {
            Ok(())
        } else {
            Err(format!("{name} has no option {option:?}; options: {chosen}").into())
        }
    }

    /// Press and release a named key (`Enter`, `Backspace`, `Escape`, ...).
    pub async fn key(&self, key: &str) -> Result<()> {
        let (code, text) = match key {
            "Enter" => (13, "\r"),
            "Backspace" => (8, ""),
            "Escape" => (27, ""),
            "Tab" => (9, ""),
            other => return Err(format!("unsupported key {other}").into()),
        };
        for kind in ["keyDown", "keyUp"] {
            let mut event =
                json!({"type": kind, "key": key, "code": key, "windowsVirtualKeyCode": code});
            if kind == "keyDown" && !text.is_empty() {
                event["text"] = json!(text);
            }
            self.call("Input.dispatchKeyEvent", event).await?;
        }
        Ok(())
    }

    /// Run `function` (JavaScript source) with `this` bound to `element`.
    pub async fn on(&self, element: Element, function: &str) -> Result<Value> {
        let resolved = self
            .call("DOM.resolveNode", json!({"backendNodeId": element.node}))
            .await?;
        let object = resolved["object"]["objectId"]
            .as_str()
            .ok_or("element is gone")?;
        let reply = self
            .call(
                "Runtime.callFunctionOn",
                json!({"objectId": object, "functionDeclaration": function, "returnByValue": true, "awaitPromise": true}),
            )
            .await?;
        if let Some(details) = reply.get("exceptionDetails") {
            return Err(format!("element call failed: {}", exception_text(details)).into());
        }
        Ok(reply["result"]["value"].clone())
    }

    /// Console errors, uncaught exceptions, failed requests and HTTP error
    /// responses seen since launch or the last [`Browser::take_problems`].
    pub fn problems(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|state| state.problems.clone())
            .unwrap_or_default()
    }

    /// Return and clear the recorded problems, for a step that expects one.
    pub fn take_problems(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|mut state| std::mem::take(&mut state.problems))
            .unwrap_or_default()
    }

    /// Fail unless no problem was recorded.
    pub fn expect_clean(&self, step: &str) -> Result<()> {
        let problems = self.problems();
        if problems.is_empty() {
            Ok(())
        } else {
            Err(format!("{step}: page problems:\n  {}", problems.join("\n  ")).into())
        }
    }

    /// Save a PNG of the viewport.
    pub async fn screenshot(&self, path: &Path) -> Result<()> {
        let shot = self
            .call("Page.captureScreenshot", json!({"format": "png"}))
            .await?;
        let data = shot["data"].as_str().ok_or("no screenshot data")?;
        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory)?;
        }
        std::fs::write(path, STANDARD.decode(data)?)?;
        Ok(())
    }

    /// `error` with the screenshot path, URL, problems and visible text.
    async fn report(&self, name: &str, error: &Error) -> String {
        let path = artifacts().map(|directory| directory.join(format!("{name}.png")));
        let shot = match &path {
            Ok(path) => match self.screenshot(path).await {
                Ok(()) => format!("screenshot: {}", path.display()),
                Err(failure) => format!("no screenshot: {failure}"),
            },
            Err(failure) => format!("no screenshot: {failure}"),
        };
        let url = self
            .url()
            .await
            .unwrap_or_else(|failure| failure.to_string());
        let text = self
            .text()
            .await
            .unwrap_or_else(|failure| failure.to_string());
        let excerpt: String = text.chars().take(TEXT_EXCERPT).collect();
        let problems = self.problems();
        let problems = if problems.is_empty() {
            "none".to_owned()
        } else {
            format!("\n  {}", problems.join("\n  "))
        };
        format!(
            "UI smoke {name} failed: {error}\n{shot}\nurl: {url}\npage problems: {problems}\nvisible text:\n{excerpt}"
        )
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        if let Some(pid) = self.child.id().and_then(|pid| Pid::from_raw(pid as i32)) {
            let _ = kill_process_group(pid, Signal::KILL);
        }
        let _ = self.child.start_kill();
        self.cdp.reader.abort();
        // Chrome may still be closing files in the profile for a moment.
        for _ in 0..20 {
            if std::fs::remove_dir_all(&self.profile).is_ok() || !self.profile.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

/// CDP errors a navigation causes: the old page's context or nodes are gone
/// before the new page has its own.
fn navigating(error: &Error) -> bool {
    let error = error.to_string();
    [
        "Cannot find context with specified id",
        "Execution context was destroyed",
        "Inspected target navigated or closed",
        "No node with given id",
        "Could not find node with given id",
    ]
    .iter()
    .any(|message| error.contains(message))
}

/// Run a page read again while a navigation settles, so waits see the new
/// page instead of failing on the old one. Other errors return at once.
async fn settled<T, F, Fut>(mut read: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut attempts = 0;
    loop {
        match read().await {
            Err(error) if navigating(&error) && attempts < 40 => {
                attempts += 1;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            result => return result,
        }
    }
}

fn find_browser() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("MINARBOT_CHROME") {
        let path = PathBuf::from(path);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!("MINARBOT_CHROME={} is not a file", path.display()).into())
        };
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .flat_map(|directory| BROWSERS.iter().map(move |name| directory.join(name)))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            format!(
                "no browser for the UI smokes: install Chrome or Chromium ({} on PATH), \
                 set MINARBOT_CHROME, or set MINARBOT_SKIP_BROWSER=1 to skip them",
                BROWSERS.join(", ")
            )
            .into()
        })
}

fn artifacts() -> Result<PathBuf> {
    match std::env::var_os("MINARBOT_SMOKE_ARTIFACTS") {
        Some(directory) => Ok(PathBuf::from(directory)),
        None => Ok(fakes::target_dir()?.join("smoke-artifacts")),
    }
}

fn exception_text(details: &Value) -> String {
    details["exception"]["description"]
        .as_str()
        .or_else(|| details["text"].as_str())
        .unwrap_or("exception")
        .to_owned()
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

/// NUL-framed JSON CDP messages over the browser pipe.
struct Cdp {
    send: tokio::sync::Mutex<(OwnedWriteHalf, u64)>,
    pending: Pending,
    reader: JoinHandle<()>,
}

impl Cdp {
    fn start(stream: UnixStream, state: Arc<Mutex<PageState>>) -> Self {
        let (receive, send) = stream.into_split();
        let pending: Pending = Arc::default();
        let replies = pending.clone();
        let reader = tokio::spawn(async move {
            let mut input = BufReader::new(receive);
            let mut packet = Vec::new();
            loop {
                packet.clear();
                match input.read_until(0, &mut packet).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if packet.last() == Some(&0) {
                    packet.pop();
                }
                let Ok(message) = serde_json::from_slice::<Value>(&packet) else {
                    continue;
                };
                if let Some(id) = message["id"].as_u64() {
                    let waiter = replies.lock().ok().and_then(|mut map| map.remove(&id));
                    if let Some(waiter) = waiter {
                        let _ = waiter.send(message);
                    }
                } else if let Ok(mut state) = state.lock() {
                    state.record(&message);
                }
            }
            // Dropping the senders fails every pending call.
            if let Ok(mut map) = replies.lock() {
                map.clear();
            }
        });
        Self {
            send: tokio::sync::Mutex::new((send, 0)),
            pending,
            reader,
        }
    }

    async fn call(&self, session: Option<&str>, method: &str, params: Value) -> Result<Value> {
        let (answer, reply) = oneshot::channel();
        {
            let mut send = self.send.lock().await;
            send.1 += 1;
            let id = send.1;
            let mut command = json!({"id": id, "method": method, "params": params});
            if let Some(session) = session {
                command["sessionId"] = json!(session);
            }
            self.pending
                .lock()
                .map_err(|_| "CDP state poisoned")?
                .insert(id, answer);
            let mut bytes = command.to_string().into_bytes();
            bytes.push(0);
            send.0
                .write_all(&bytes)
                .await
                .map_err(|error| format!("{method}: browser pipe closed: {error}"))?;
        }
        let message = tokio::time::timeout(WAIT, reply)
            .await
            .map_err(|_| format!("{method}: no reply within {WAIT:?}"))?
            .map_err(|_| format!("{method}: browser closed"))?;
        match message.get("error") {
            Some(error) => Err(format!("{method}: {error}").into()),
            None => Ok(message["result"].clone()),
        }
    }
}

impl PageState {
    fn record(&mut self, event: &Value) {
        let params = &event["params"];
        let problem = match event["method"].as_str().unwrap_or_default() {
            "Page.loadEventFired" => {
                self.loads += 1;
                None
            }
            "Network.requestWillBeSent" => {
                if let (Some(id), Some(url)) = (
                    params["requestId"].as_str(),
                    params["request"]["url"].as_str(),
                ) {
                    let method = params["request"]["method"].as_str().unwrap_or("GET");
                    self.requests.insert(id.into(), format!("{method} {url}"));
                }
                None
            }
            "Network.responseReceived" => {
                let status = params["response"]["status"].as_u64().unwrap_or_default();
                (status >= 400).then(|| {
                    let request = self.request(params);
                    format!("HTTP {status}: {request}")
                })
            }
            // A cancelled request (navigation, closed stream) is not a failure.
            "Network.loadingFailed" if params["canceled"] != true => Some(format!(
                "request failed ({}): {}",
                params["errorText"].as_str().unwrap_or("error"),
                self.request(params)
            )),
            "Runtime.exceptionThrown" => Some(format!(
                "uncaught: {}",
                exception_text(&params["exceptionDetails"])
            )),
            "Runtime.consoleAPICalled"
                if matches!(params["type"].as_str(), Some("error" | "assert")) =>
            {
                let text: Vec<String> = params["args"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|argument| {
                        argument["value"]
                            .as_str()
                            .map(str::to_owned)
                            .or_else(|| argument["description"].as_str().map(str::to_owned))
                            .unwrap_or_else(|| argument["value"].to_string())
                    })
                    .collect();
                Some(format!("console.error: {}", text.join(" ")))
            }
            // Network entries repeat the network events above.
            "Log.entryAdded"
                if params["entry"]["level"] == "error"
                    && params["entry"]["source"] != "network" =>
            {
                Some(format!(
                    "log: {} {}",
                    params["entry"]["text"].as_str().unwrap_or_default(),
                    params["entry"]["url"].as_str().unwrap_or_default()
                ))
            }
            _ => None,
        };
        self.problems.extend(problem);
    }

    fn request(&self, params: &Value) -> String {
        params["requestId"]
            .as_str()
            .and_then(|id| self.requests.get(id))
            .cloned()
            .or_else(|| params["response"]["url"].as_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown request".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn settled_retries_navigation_errors_and_returns_others_at_once() -> Result<()> {
        let calls = AtomicUsize::new(0);
        let value = settled(|| async {
            if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                Err::<u8, Error>(
                    r#"Accessibility.queryAXTree: {"code":-32000,"message":"Cannot find context with specified id"}"#
                        .into(),
                )
            } else {
                Ok(7)
            }
        })
        .await?;
        crate::ensure!(
            value == 7 && calls.load(Ordering::SeqCst) == 3,
            "retried read"
        );

        let calls = AtomicUsize::new(0);
        let failed = settled(|| async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err::<u8, Error>("Page.navigate: net::ERR_CONNECTION_REFUSED".into())
        })
        .await;
        crate::ensure!(
            failed.is_err() && calls.load(Ordering::SeqCst) == 1,
            "other errors return at once"
        );
        Ok(())
    }
}
