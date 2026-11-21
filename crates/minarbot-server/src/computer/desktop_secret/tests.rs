use super::*;
use crate::db::Result;
use std::process::Stdio;

#[tokio::test]
#[ignore = "needs installed Google Chrome"]
async fn chrome_fills_only_the_original_focused_field() -> Result<()> {
    use axum::{response::Html, routing::get, Router};
    use std::{fs, path::PathBuf, time::Duration};
    const SENSITIVE: &str = "synthetic-fixture-value";
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let scratch = Scratch(
        std::env::temp_dir().join(format!("minarbot-vault-browser-{}", uuid::Uuid::new_v4())),
    );
    fs::create_dir(&scratch.0)?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().fallback(get(|| async {
            Html(r#"<input id="first" autofocus><input id="second"><script>
                first.addEventListener('input', () => document.title = first.value === 'synthetic-fixture-value' ? 'matched' : 'wrong');
            </script>"#)
        }))).await
    });
    let mut command = tokio::process::Command::new("sh");
    let mut chrome = Connection::configure(&mut command, "google-chrome")?;
    let mut child = command
        .args([
            "--headless",
            "--no-sandbox",
            "--disable-background-networking",
            "--disable-gpu",
            "--no-first-run",
            "--no-default-browser-check",
            "--no-proxy-server",
        ])
        .arg(format!("--user-data-dir={}", scratch.0.display()))
        .arg(&origin)
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    drop(command);
    let result = tokio::time::timeout(Duration::from_secs(45), async {
        {
            let io = &mut chrome;
            let tab = loop {
                let result = match io.call(None, "Target.getTargets", json!({})).await {
                    Err(Error::Transport) => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                    result => result?,
                };
                if let Some(tab) = result["targetInfos"]
                    .as_array()
                    .and_then(|tabs| tabs.iter().find(|tab| tab["type"] == "page"))
                    .and_then(|tab| tab["targetId"].as_str())
                {
                    break tab.to_owned();
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            };
            let attached = io
                .call(
                    None,
                    "Target.attachToTarget",
                    json!({"targetId":tab,"flatten":true}),
                )
                .await?;
            let session = attached["sessionId"]
                .as_str()
                .ok_or("Fixture tab unavailable.")?;
            io.call(Some(session), "Page.bringToFront", json!({}))
                .await?;
            loop {
                let ready = io
                    .call(Some(session), "Runtime.evaluate", json!({
                        "expression":"(() => { const field = document.getElementById('first'); if (!field?.getClientRects().length) return false; field.focus(); return document.hasFocus() && document.activeElement === field; })()",
                        "returnByValue":true
                    }))
                    .await;
                if ready.is_ok_and(|value| value["result"]["value"] == true) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            io.call(
                None,
                "Target.detachFromTarget",
                json!({"sessionId":session}),
            )
            .await?;
        }
        assert!(chrome.focused(&origin, "fixture-target", None).await?);
        assert!(
            !chrome
                .focused("https://example.invalid", "fixture-target", Some(SENSITIVE))
                .await?
        );
        assert!(
            !chrome
                .focused(&origin, "different-target", Some(SENSITIVE))
                .await?
        );
        assert!(
            chrome
                .focused(&origin, "fixture-target", Some(SENSITIVE))
                .await?
        );
        assert!(
            !chrome
                .focused(&origin, "fixture-target", Some(SENSITIVE))
                .await?
        );
        let targets = chrome.call(None, "Target.getTargets", json!({})).await?;
        assert!(targets["targetInfos"]
            .as_array()
            .is_some_and(|tabs| tabs.iter().any(|tab| tab["title"] == "matched")));
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await;
    child.kill().await?;
    server.abort();
    result??;
    Ok(())
}
