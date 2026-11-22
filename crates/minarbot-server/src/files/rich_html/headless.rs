use super::*;
use crate::computer::desktop_supervisor::cdp::Connection as Chrome;
use base64::{engine::general_purpose::STANDARD, Engine};
use sha2::{Digest, Sha256};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{process::Command, sync::Semaphore, time::timeout};

static PREVIEWS: Semaphore = Semaphore::const_new(2);
const UNAVAILABLE: &str =
    "HTML preview requires Chrome or Chromium with a working bubblewrap sandbox.";
const DIAGNOSTICS: &str = r#"<script>
(() => {
  const entries = [];
  Object.defineProperty(window, '__minarbotConsole', { value: entries });
  const record = (level, text) => {
    if (entries.length < 20) entries.push({ level, text: String(text).slice(0, 500) });
  };
  for (const method of ['log', 'info', 'warn', 'error']) {
    const original = console[method];
    console[method] = (...args) => {
      if (entries.length < 20) record(method === 'warn' ? 'warning' : method, args.slice(0, 5).map(String).join(' '));
      original.apply(console, args);
    };
  }
  addEventListener('error', event => record('error', event.message || 'Resource failed to load'), true);
  addEventListener('unhandledrejection', event => record('error', event.reason));
  addEventListener('securitypolicyviolation', event => record('warning', 'Blocked by policy: ' + event.violatedDirective));
})();
</script>"#;

async fn assets(source: &str) -> Result<String> {
    let mut html = source.to_owned();
    for (url, digest) in LIBRARIES.into_iter().zip(LIBRARY_DIGESTS) {
        if source.contains(url) {
            let bytes = crate::turns::web::pinned_script(url).await?;
            if format!("{:x}", Sha256::digest(&bytes)) != digest {
                return Err(error(
                    502,
                    "Pinned HTML preview script failed integrity verification.",
                ));
            }
            let data = format!("data:text/javascript;base64,{}", STANDARD.encode(bytes));
            // Replace one copy per dependency to bound expanded markup.
            html = html.replacen(url, &data, 1);
        }
    }
    Ok(html)
}

pub(super) async fn capture(source: String, width: u64, appearance: &str) -> Result<Value> {
    let _permit = PREVIEWS
        .try_acquire()
        .map_err(|_| error(429, "HTML previews are busy. Try again shortly."))?;
    timeout(Duration::from_secs(15), async {
        let html = assets(&source).await?;
        run(
            &document(&format!("{DIAGNOSTICS}{html}"), Some(appearance)),
            width,
            appearance,
        )
        .await
    })
    .await
    .map_err(|_| error(504, "HTML preview timed out."))?
}

async fn run(html: &str, width: u64, appearance: &str) -> Result<Value> {
    let browser = [
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/usr/bin/chromium",
    ]
    .iter()
    .find_map(|path| std::fs::canonicalize(path).ok())
    .filter(|path| path.starts_with("/usr") || path.starts_with("/opt/google/chrome"))
    .ok_or_else(|| error(503, UNAVAILABLE))?;
    let browser = browser.to_str().ok_or_else(|| error(503, UNAVAILABLE))?;
    let mut command = Command::new("/usr/bin/bwrap");
    command.args([
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--clearenv",
        "--ro-bind",
        "/usr",
        "/usr",
        "--symlink",
        "usr/bin",
        "/bin",
        "--symlink",
        "usr/lib",
        "/lib",
        "--symlink",
        "usr/lib64",
        "/lib64",
        "--dev",
        "/dev",
        "--proc",
        "/proc",
        "--tmpfs",
        "/tmp",
        "--setenv",
        "HOME",
        "/tmp",
        "--setenv",
        "PATH",
        "/usr/bin:/bin",
        "--setenv",
        "LANG",
        "C.UTF-8",
    ]);
    for path in ["/opt/google/chrome", "/etc/fonts"] {
        if Path::new(path).is_dir() {
            command.args(["--ro-bind", path, path]);
        }
    }
    command
        .args(["--", "/bin/sh"])
        .env_clear()
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut chrome =
        Chrome::configure(&mut command, browser).map_err(|_| error(503, UNAVAILABLE))?;
    let mut child = command
        .args([
            "--headless",
            "--user-data-dir=/tmp/profile",
            "--disable-dev-shm-usage",
            "--disable-gpu",
            "--disable-background-networking",
            "--disable-extensions",
            "--no-first-run",
            "--hide-scrollbars",
            "about:blank",
        ])
        .spawn()
        .map_err(|_| error(503, UNAVAILABLE))?;

    let result = capture_page(&mut chrome, html, width, appearance).await;
    let _ = child.kill().await;
    result
}

async fn capture_page(
    chrome: &mut Chrome,
    html: &str,
    width: u64,
    appearance: &str,
) -> Result<Value> {
    let start = |_: crate::computer::desktop_supervisor::cdp::Error| error(503, UNAVAILABLE);
    let fail = |_: crate::computer::desktop_supervisor::cdp::Error| {
        error(422, "Chrome could not finish the HTML preview.")
    };
    let targets = chrome
        .call(None, "Target.getTargets", json!({}))
        .await
        .map_err(start)?;
    let target = targets["targetInfos"]
        .as_array()
        .and_then(|targets| targets.iter().find(|t| t["type"] == "page"))
        .and_then(|t| t["targetId"].as_str())
        .ok_or_else(|| error(503, UNAVAILABLE))?;
    let attach = chrome
        .call(
            None,
            "Target.attachToTarget",
            json!({"targetId":target,"flatten":true}),
        )
        .await
        .map_err(start)?;
    let session = Some(
        attach["sessionId"]
            .as_str()
            .ok_or_else(|| error(503, UNAVAILABLE))?,
    );
    chrome
        .call(
            session,
            "Emulation.setDeviceMetricsOverride",
            json!({"width":width,"height":80,"deviceScaleFactor":1,"mobile":false}),
        )
        .await
        .map_err(fail)?;
    chrome
        .call(
            session,
            "Emulation.setEmulatedMedia",
            json!({"features":[{"name":"prefers-color-scheme","value":appearance}]}),
        )
        .await
        .map_err(fail)?;
    let tree = chrome
        .call(session, "Page.getFrameTree", json!({}))
        .await
        .map_err(fail)?;
    chrome
        .call(
            session,
            "Page.setDocumentContent",
            json!({"frameId":tree["frameTree"]["frame"]["id"],"html":html}),
        )
        .await
        .map_err(fail)?;
    let measured = chrome.call(session, "Runtime.evaluate", json!({
        "expression": "(async () => { await document.fonts.ready; await new Promise(resolve => setTimeout(resolve, 150)); const root = document.documentElement; return { height: Math.ceil(root.scrollHeight > root.clientHeight ? root.scrollHeight : root.getBoundingClientRect().height), console: window.__minarbotConsole }; })()",
        "awaitPromise":true,"returnByValue":true,
    })).await.map_err(fail)?;
    let measurement = &measured["result"]["value"];
    let height = measurement["height"]
        .as_u64()
        .filter(|height| *height > 0)
        .ok_or_else(|| error(422, "HTML preview could not measure the page."))?;
    let captured = height.clamp(80, 2000);
    chrome
        .call(
            session,
            "Emulation.setDeviceMetricsOverride",
            json!({"width":width,"height":captured,"deviceScaleFactor":1,"mobile":false}),
        )
        .await
        .map_err(fail)?;
    let png = chrome
        .call(
            session,
            "Page.captureScreenshot",
            json!({"format":"png","fromSurface":true}),
        )
        .await
        .map_err(fail)?;
    let data = png["data"]
        .as_str()
        .filter(|s| s.len() <= MCP_PREVIEW_LIMIT - 64 * 1024)
        .ok_or_else(|| error(413, "HTML preview screenshot exceeds the size limit."))?;
    let console = measurement["console"]
        .as_array()
        .into_iter()
        .flatten()
        .take(20)
        .filter_map(|entry| {
            let level = entry["level"]
                .as_str()
                .filter(|level| matches!(*level, "log" | "info" | "warning" | "error"))?;
            let text: String = entry["text"].as_str()?.chars().take(500).collect();
            Some(json!({"level":level,"text":text}))
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"width":width,"contentHeight":height,"capturedHeight":captured,"consoleMessages":console,"screenshot":{"mimeType":"image/png","data":data,"width":width,"height":captured}}),
    )
}
