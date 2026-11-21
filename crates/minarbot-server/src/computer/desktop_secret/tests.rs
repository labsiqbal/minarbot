use super::*;

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
    let chrome = ChromePipe::configure(&mut command, "google-chrome")?;
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
        .arg("about:blank")
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let result = tokio::time::timeout(Duration::from_secs(45), async {
        {
            let mut io = chrome.io.lock().await;
            let tab = loop {
                let result = io.call(None, "Target.getTargets", json!({})).await?;
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
            io.call(Some(session), "Page.enable", json!({})).await?;
            let response = io
                .call(Some(session), "Page.navigate", json!({"url":origin}))
                .await?;
            assert!(response.get("errorText").is_none());
            io.call(Some(session), "Page.bringToFront", json!({}))
                .await?;
            for event in ["mousePressed", "mouseReleased"] {
                io.call(
                    Some(session),
                    "Input.dispatchMouseEvent",
                    json!({
                        "type":event,"x":16,"y":16,"button":"left","clickCount":1
                    }),
                )
                .await?;
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
        let targets = chrome
            .io
            .lock()
            .await
            .call(None, "Target.getTargets", json!({}))
            .await?;
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
