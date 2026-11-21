use super::desktop_supervisor::cdp::{Connection, Error, Result};
use serde_json::json;

impl Connection {
    pub(in crate::computer) async fn focused(
        &mut self,
        origin: &str,
        target: &str,
        value: Option<&str>,
    ) -> Result<bool> {
        let targets = self.call(None, "Target.getTargets", json!({})).await?;
        let targets = targets["targetInfos"].as_array().ok_or(Error::Command)?;
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
            let attached = self
                .call(
                    None,
                    "Target.attachToTarget",
                    json!({"targetId":id,"flatten":true}),
                )
                .await?;
            let session = attached["sessionId"].as_str().ok_or(Error::Command)?;
            let result = self
                .focused_in_session(session, origin, target, value)
                .await;
            let _ = self
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

    async fn focused_in_session(
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
            .ok_or(Error::Command)?;
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
            .ok_or(Error::Command)?;
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
            return Err(Error::Command);
        }
        Ok(response["result"]["value"] == true)
    }
}

#[cfg(test)]
mod tests;
