//! The app's own Android code (`NativePlugin.kt`): push registration, the
//! system bar colours and the resume signal. It exposes no JavaScript commands,
//! so no webview can call it; only Rust does.

use tauri::{
    plugin::{Builder, TauriPlugin},
    AppHandle, Manager, WebviewWindow, Wry,
};

#[cfg(target_os = "android")]
pub(crate) struct Native(pub(crate) tauri::plugin::PluginHandle<Wry>);

#[cfg(target_os = "android")]
pub(crate) fn handle(app: &AppHandle) -> Option<tauri::plugin::PluginHandle<Wry>> {
    app.try_state::<Native>().map(|native| native.0.clone())
}

pub fn init() -> TauriPlugin<Wry> {
    Builder::<Wry>::new("minarbot-native")
        .setup(|_app, _api| {
            #[cfg(target_os = "android")]
            {
                let handle = _api.register_android_plugin("com.minarbot.mobile", "NativePlugin")?;
                _app.manage(Native(handle));
            }
            Ok(())
        })
        .build()
}

/// Calls `on_resume` each time the owner returns to the app. Tauri does not report
/// an Android activity resume as `RunEvent::Resumed`, so the plugin sends it.
#[cfg(target_os = "android")]
pub fn on_resume(app: &AppHandle, on_resume: impl Fn(&AppHandle) + Send + Sync + 'static) {
    let Some(handle) = handle(app) else {
        return;
    };
    let resumed = app.clone();
    let channel = tauri::ipc::Channel::<serde_json::Value>::new(move |_| {
        on_resume(&resumed);
        Ok(())
    });
    tauri::async_runtime::spawn(async move {
        if let Err(error) = handle
            .run_mobile_plugin_async::<serde_json::Value>(
                "registerListener",
                serde_json::json!({ "event": "resume", "handler": channel }),
            )
            .await
        {
            log::error!("Could not listen for resume: {error}");
        }
    });
}

#[cfg(not(target_os = "android"))]
pub fn on_resume(_app: &AppHandle, _on_resume: impl Fn(&AppHandle) + Send + Sync + 'static) {}

/// The colour at the top of the page, as `[r, g, b]`: the first opaque background
/// under the top edge, or the canvas white.
const TOP_COLOUR: &str = "(() => {
  for (let node = document.elementFromPoint(innerWidth / 2, 1); node; node = node.parentElement) {
    const parts = (getComputedStyle(node).backgroundColor.match(/[\\d.]+/g) || []).map(Number);
    if (parts.length >= 3 && (parts.length < 4 || parts[3] > 0)) return parts.slice(0, 3);
  }
  return [255, 255, 255];
})()";

/// Bar icon style and ARGB colour for a `TOP_COLOUR` result: dark icons over a light colour.
fn bar_style(result: &str) -> Option<(bool, i32)> {
    let rgb: Vec<f64> = serde_json::from_str(result).ok()?;
    let [r, g, b] = <[f64; 3]>::try_from(rgb)
        .ok()?
        .map(|part| part.clamp(0.0, 255.0));
    let light = 0.299 * r + 0.587 * g + 0.114 * b > 140.0;
    let argb = 0xFF00_0000_u32 | (r as u32) << 16 | (g as u32) << 8 | b as u32;
    Some((light, i32::from_ne_bytes(argb.to_ne_bytes())))
}

/// Paints the system bars like the top of the page, now and once the page has
/// rendered (the web client paints after its script loads).
pub fn match_system_bars(window: &WebviewWindow) {
    check_bars(window);
    let window = window.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        check_bars(&window);
    });
}

fn check_bars(window: &WebviewWindow) {
    let app = window.app_handle().clone();
    let result = window.eval_with_callback(TOP_COLOUR, move |colour| {
        if let Some((light, colour)) = bar_style(&colour) {
            set_system_bars(&app, light, colour);
        }
    });
    if let Err(error) = result {
        log::warn!("Could not read the page colour: {error}");
    }
}

#[cfg(target_os = "android")]
fn set_system_bars(app: &AppHandle, light: bool, colour: i32) {
    let Some(handle) = handle(app) else {
        return;
    };
    tauri::async_runtime::spawn(async move {
        if let Err(error) = handle
            .run_mobile_plugin_async::<serde_json::Value>(
                "systemBars",
                serde_json::json!({ "light": light, "color": colour }),
            )
            .await
        {
            log::warn!("Could not set the system bars: {error}");
        }
    });
}

#[cfg(not(target_os = "android"))]
fn set_system_bars(_app: &AppHandle, _light: bool, _colour: i32) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_contrast_with_the_page_top() -> crate::connection::Result<()> {
        assert_eq!(bar_style("[17,17,19]"), Some((false, -0x00EE_EEED)));
        assert_eq!(
            bar_style("[248, 248, 250]").map(|style| style.0),
            Some(true)
        );
        assert_eq!(bar_style("[255,255,255]"), Some((true, -1)));
        for invalid in ["null", "{}", "[1,2]", "\"#fff\""] {
            assert_eq!(bar_style(invalid), None, "{invalid}");
        }
        Ok(())
    }
}
