//! Push registration. The Android side (`NativePlugin.kt`) reads the FCM token
//! when the build carries a Firebase config; without one it reports why and the
//! app works without push.
//!
//! The token reaches the server through the server's own page: Rust runs a fixed
//! `fetch` there, so it travels with the page's paired-device session, and the
//! page keeps zero Tauri capabilities.

use crate::connection::Result;
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, sync::Mutex};
use tauri::{AppHandle, Manager, Url, WebviewWindow};

/// What the native side reports about push on this device.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Registration {
    /// The FCM registration token, absent when push is unavailable.
    pub token: Option<String>,
    /// Why push is unavailable, for the log.
    pub reason: Option<String>,
    /// The device's model name, shown in the server's device list.
    pub name: Option<String>,
    /// The route of the notification that launched the app.
    pub opened: Option<String>,
}

#[derive(Default)]
pub struct Push {
    registration: Mutex<Option<Registration>>,
    /// Server origin and token pairs already sent in this run.
    delivered: Mutex<HashSet<(String, String)>>,
    /// The server page that loaded before the token arrived.
    waiting: Mutex<Option<(WebviewWindow, Url)>>,
    /// The server page shown last, for a token that rotates later.
    shown: Mutex<Option<(WebviewWindow, Url)>>,
}

/// The body `POST /api/push/devices` takes (see labsiqbal/minarbot#109).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Device<'a> {
    id: String,
    platform: &'static str,
    token: &'a str,
    name: &'a str,
    app_id: &'a str,
    environment: &'static str,
}

const PLATFORM: &str = if cfg!(target_os = "ios") {
    "ios"
} else {
    "android"
};

/// The script the server page runs: register this device, then drop the one it replaces.
fn script(device: &Device, delete_id: &str) -> Result<String> {
    Ok(format!(
        "(async () => {{
  const key = 'minarbot.push.device';
  try {{
    const response = await fetch('/api/push/devices', {{
      method: 'POST', credentials: 'same-origin',
      headers: {{ 'Content-Type': 'application/json' }},
      body: JSON.stringify({device}),
    }});
    const reply = await response.json().catch(() => ({{}}));
    if (!response.ok) throw new Error(reply.error || `HTTP ${{response.status}}`);
    const previous = localStorage.getItem(key);
    const current = reply.device && reply.device.id;
    if (previous && current && previous !== current) {{
      await fetch(`/api/push/devices/${{encodeURIComponent(previous)}}/delete`, {{
        method: 'POST', credentials: 'same-origin',
        headers: {{ 'Content-Type': 'application/json' }},
        body: JSON.stringify({{ id: {delete_id} }}),
      }});
    }}
    if (current) localStorage.setItem(key, current);
    console.info('Minarbot push: registered', current, reply.configured);
  }} catch (error) {{
    console.warn('Minarbot push: registration failed', String(error));
  }}
}})();",
        device = serde_json::to_string(device)?,
        delete_id = serde_json::to_string(delete_id)?,
    ))
}

impl Push {
    fn record(&self, registration: Registration) -> Result<()> {
        match (&registration.token, &registration.reason) {
            (Some(_), _) => log::info!("Push: token ready"),
            (None, reason) => log::warn!(
                "Push: registration skipped: {}",
                reason.as_deref().unwrap_or("no token")
            ),
        }
        *self
            .registration
            .lock()
            .map_err(|_| "Push state lock poisoned.")? = Some(registration);
        let waiting = self
            .waiting
            .lock()
            .map_err(|_| "Push state lock poisoned.")?
            .take();
        if let Some((window, server)) = waiting {
            self.deliver(&window, &server);
        }
        Ok(())
    }

    /// A rotated token: register it with the server the phone shows now.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    fn refresh(&self, registration: Registration) -> Result<()> {
        let current = self
            .registration
            .lock()
            .map_err(|_| "Push state lock poisoned.")?
            .clone()
            .unwrap_or_default();
        if registration.token.is_none() || registration.token == current.token {
            return Ok(());
        }
        let shown = self
            .shown
            .lock()
            .map_err(|_| "Push state lock poisoned.")?
            .clone();
        *self
            .registration
            .lock()
            .map_err(|_| "Push state lock poisoned.")? = Some(Registration {
            opened: None,
            name: registration.name.or(current.name),
            ..registration
        });
        log::info!("Push: token rotated");
        if let Some((window, server)) = shown {
            self.deliver(&window, &server);
        }
        Ok(())
    }

    /// Sends the token to the server shown in `window`, once per server and token.
    pub fn deliver(&self, window: &WebviewWindow, server: &Url) {
        if let Err(error) = self.try_deliver(window, server) {
            log::error!("Push: could not register with the server: {error}");
        }
    }

    fn try_deliver(&self, window: &WebviewWindow, server: &Url) -> Result<()> {
        *self.shown.lock().map_err(|_| "Push state lock poisoned.")? =
            Some((window.clone(), server.clone()));
        let registration = self
            .registration
            .lock()
            .map_err(|_| "Push state lock poisoned.")?
            .clone();
        let Some(registration) = registration else {
            *self
                .waiting
                .lock()
                .map_err(|_| "Push state lock poisoned.")? = Some((window.clone(), server.clone()));
            return Ok(());
        };
        let Some(token) = registration.token else {
            return Ok(());
        };
        let key = (server.origin().ascii_serialization(), token.clone());
        if !self
            .delivered
            .lock()
            .map_err(|_| "Push state lock poisoned.")?
            .insert(key)
        {
            return Ok(());
        }
        let app_id = window.app_handle().config().identifier.clone();
        let device = Device {
            id: uuid::Uuid::new_v4().to_string(),
            platform: PLATFORM,
            token: &token,
            name: registration.name.as_deref().unwrap_or("Phone"),
            app_id: &app_id,
            environment: "production",
        };
        window.eval(script(&device, &uuid::Uuid::new_v4().to_string())?)?;
        log::info!(
            "Push: registering with {}",
            server.origin().ascii_serialization()
        );
        Ok(())
    }
}

/// Asks the native side for a token in the background; the result lands in [`Push`].
pub fn register(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let registration = native(&app).await.unwrap_or_else(|error| Registration {
            reason: Some(error.to_string()),
            ..Registration::default()
        });
        if let Some(path) = &registration.opened {
            crate::open_route(&app, path);
        }
        if let Err(error) = app.state::<crate::Phone>().push.record(registration) {
            log::error!("Push: {error}");
        }
    });
}

#[cfg(target_os = "android")]
async fn native(app: &AppHandle) -> Result<Registration> {
    use tauri::ipc::{Channel, InvokeResponseBody};
    let handle = crate::native::handle(app).ok_or("The native plugin is not loaded.")?;
    let body = |body: InvokeResponseBody| -> Option<serde_json::Value> {
        match body {
            InvokeResponseBody::Json(json) => serde_json::from_str(&json).ok(),
            InvokeResponseBody::Raw(_) => None,
        }
    };
    let tapped = app.clone();
    let tap = Channel::<serde_json::Value>::new(move |message| {
        if let Some(path) = body(message).and_then(|value| value["path"].as_str().map(String::from))
        {
            crate::open_route(&tapped, &path);
        }
        Ok(())
    });
    let refreshed = app.clone();
    let token = Channel::<serde_json::Value>::new(move |message| {
        if let Some(value) = body(message) {
            if let Ok(registration) = serde_json::from_value::<Registration>(value) {
                let push = &refreshed.state::<crate::Phone>().push;
                if let Err(error) = push.refresh(registration) {
                    log::error!("Push: {error}");
                }
            }
        }
        Ok(())
    });
    for (event, handler) in [("tap", tap), ("token", token)] {
        handle
            .run_mobile_plugin_async::<serde_json::Value>(
                "registerListener",
                serde_json::json!({ "event": event, "handler": handler }),
            )
            .await?;
    }
    Ok(handle
        .run_mobile_plugin_async::<Registration>("register", ())
        .await?)
}

#[cfg(not(target_os = "android"))]
async fn native(_app: &AppHandle) -> Result<Registration> {
    Ok(Registration {
        reason: Some("Push is not set up on this platform yet.".into()),
        ..Registration::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_script_posts_the_contract_body() -> Result<()> {
        let device = Device {
            id: "00000000-0000-4000-8000-000000000001".into(),
            platform: "android",
            token: "token'</script>",
            name: "Pixel 8",
            app_id: "com.minarbot.mobile",
            environment: "production",
        };
        let script = script(&device, "00000000-0000-4000-8000-000000000002")?;
        assert!(script.contains(
            r#"body: JSON.stringify({"id":"00000000-0000-4000-8000-000000000001","platform":"android","token":"token'</script>","name":"Pixel 8","appId":"com.minarbot.mobile","environment":"production"})"#
        ));
        assert!(script.contains("fetch('/api/push/devices', {"));
        assert!(script.contains(r#"{ id: "00000000-0000-4000-8000-000000000002" }"#));
        Ok(())
    }

    #[test]
    fn native_reply_without_firebase_has_a_reason() -> Result<()> {
        let reply: Registration = serde_json::from_str(
            r#"{"reason":"No Firebase config in this build.","name":"sdk_gphone64_x86_64"}"#,
        )?;
        assert_eq!(reply.token, None);
        assert_eq!(
            reply.reason.as_deref(),
            Some("No Firebase config in this build.")
        );
        Ok(())
    }
}
