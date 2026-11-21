// Chrome DevTools pipe stays between the supervisor and Chrome. No debugging port,
// clipboard or secret-bearing command line is needed.
use serde_json::{json, Value};
use std::{os::unix::net::UnixStream, process::Stdio};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream as AsyncStream,
    sync::Mutex,
};

type Result<T> = crate::db::Result<T>;

pub(super) struct ChromePipe {
    io: Mutex<Session>,
}

struct Session {
    stream: BufReader<AsyncStream>,
    sequence: u64,
}

impl ChromePipe {
    pub(super) fn configure(command: &mut tokio::process::Command, binary: &str) -> Result<Self> {
        let (supervisor, browser) = UnixStream::pair()?;
        supervisor.set_nonblocking(true)?;
        let stdin: std::os::fd::OwnedFd = browser.try_clone()?.into();
        let stdout: std::os::fd::OwnedFd = browser.into();
        command
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(stdout))
            .args([
                "-c",
                r#"exec 3<&0 4>&1 </dev/null >/dev/null; exec "$@""#,
                "minarbot-chrome",
                binary,
                "--remote-debugging-pipe",
            ]);
        Ok(Self {
            io: Mutex::new(Session {
                stream: BufReader::new(AsyncStream::from_std(supervisor)?),
                sequence: 0,
            }),
        })
    }

    pub(super) async fn focused(
        &self,
        origin: &str,
        target: &str,
        value: Option<&str>,
    ) -> Result<bool> {
        let mut io = self.io.lock().await;
        let targets = io.call(None, "Target.getTargets", json!({})).await?;
        let targets = targets["targetInfos"]
            .as_array()
            .ok_or("Browser tabs unavailable.")?;
        for tab in targets.iter().filter(|tab| tab["type"] == "page") {
            let Some(id) = tab["targetId"].as_str() else {
                continue;
            };
            if url::Url::parse(tab["url"].as_str().unwrap_or_default())
                .ok()
                .is_none_or(|url| url.origin().ascii_serialization() != origin)
            {
                continue;
            }
            let attached = io
                .call(
                    None,
                    "Target.attachToTarget",
                    json!({"targetId":id,"flatten":true}),
                )
                .await?;
            let session = attached["sessionId"]
                .as_str()
                .ok_or("Browser tab unavailable.")?;
            let result = io.focused(session, origin, target, value).await;
            let _ = io
                .call(
                    None,
                    "Target.detachFromTarget",
                    json!({"sessionId":session}),
                )
                .await;
            if result? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) async fn close(&self) -> Result<()> {
        self.io
            .lock()
            .await
            .call(None, "Browser.close", json!({}))
            .await?;
        Ok(())
    }
}

impl Session {
    async fn call(&mut self, session: Option<&str>, method: &str, params: Value) -> Result<Value> {
        self.sequence += 1;
        let mut message = json!({"id":self.sequence,"method":method,"params":params});
        if let Some(session) = session {
            message["sessionId"] = json!(session);
        }
        let mut bytes = serde_json::to_vec(&message)?;
        bytes.push(0);
        self.stream.get_mut().write_all(&bytes).await?;
        let mut reply = Vec::new();
        loop {
            reply.clear();
            // A page can produce large DevTools events. Do not deserialize unbounded frames.
            let read = tokio::io::AsyncReadExt::take(&mut self.stream, 1024 * 1024)
                .read_until(0, &mut reply)
                .await?;
            if read == 0 || reply.pop() != Some(0) {
                return Err("Browser connection closed or response too large.".into());
            }
            let reply: Value =
                serde_json::from_slice(&reply).map_err(|_| "Invalid browser response.")?;
            if reply["id"] == self.sequence {
                return if let Some(result) = reply.get("result") {
                    Ok(result.clone())
                } else {
                    Err("Browser command failed.".into())
                };
            }
        }
    }

    async fn focused(
        &mut self,
        session: &str,
        origin: &str,
        target: &str,
        value: Option<&str>,
    ) -> Result<bool> {
        let session = Some(session);
        let tree = self.call(session, "Page.getFrameTree", json!({})).await?;
        let frame = tree["frameTree"]["frame"]["id"]
            .as_str()
            .ok_or("Browser frame unavailable.")?;
        let context = self
            .call(
                session,
                "Page.createIsolatedWorld",
                json!({
                    "frameId":frame,"worldName":"minarbot-vault"
                }),
            )
            .await?;
        let context = context["executionContextId"]
            .as_u64()
            .ok_or("Browser frame unavailable.")?;
        // Verify the live origin and field in the same evaluation as the write.
        // An isolated world keeps these checks on native DOM methods.
        let args = json!([origin, target, value]);
        let expression = format!(
            r#"(([origin, target, value]) => {{
            if (location.origin !== origin || !document.hasFocus()) return false;
            const field = document.activeElement;
            if (!(field instanceof HTMLInputElement && ['text','password','email','tel','url','number','search'].includes(field.type)
                || field instanceof HTMLTextAreaElement) || field.matches(':disabled') || field.readOnly
                || !field.getClientRects().length) return false;
            if (value === null) {{
                globalThis.minarbotVaultTarget = {{target, field}};
                return true;
            }}
            if (target && (globalThis.minarbotVaultTarget?.target !== target
                || globalThis.minarbotVaultTarget.field !== field)) return false;
            delete globalThis.minarbotVaultTarget;
            const prototype = field instanceof HTMLInputElement ? HTMLInputElement.prototype : HTMLTextAreaElement.prototype;
            const set = Object.getOwnPropertyDescriptor(prototype, 'value').set;
            const previous = field.value;
            set.call(field, value);
            if (field.value !== value) {{
                set.call(field, previous);
                return false;
            }}
            field.autocomplete = 'off';
            field.dispatchEvent(new Event('input', {{bubbles:true, composed:true}}));
            field.dispatchEvent(new Event('change', {{bubbles:true}}));
            return true;
        }})({args})"#
        );
        let response = self.call(session, "Runtime.evaluate", json!({
            "expression":expression, "contextId":context, "returnByValue":true, "silent":true
        })).await?;
        if response.get("exceptionDetails").is_some() {
            return Err("Browser could not evaluate secure fill.".into());
        }
        Ok(response["result"]["value"] == true)
    }
}

#[cfg(test)]
mod tests;
