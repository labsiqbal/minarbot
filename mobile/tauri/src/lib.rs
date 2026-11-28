//! Minarbot for phones: a bundled connect form and the server's own web client
//! in one webview. The phone never runs a server; it connects to one on the
//! owner's tailnet.

#[path = "../../../desktop/tauri/src/connection.rs"]
mod connection;
mod native;
mod push;
mod settings;

use connection::{external, origin, probe, same_origin, Result};
use settings::Settings;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex, MutexGuard,
    },
};
use tauri::{
    webview::{PageLoadEvent, WebviewWindowBuilder},
    AppHandle, Manager, Url, WebviewUrl, WebviewWindow,
};
use tauri_plugin_opener::OpenerExt;

const WINDOW: &str = "main";

struct Phone {
    path: PathBuf,
    /// The server the webview may show. Navigation to any other origin is refused.
    server: Mutex<Option<Url>>,
    /// The server the owner picked, kept while the phone shows the connect form after a failure.
    saved: Mutex<Option<Url>>,
    /// Status and error the connect form shows next.
    form: Mutex<(String, String)>,
    generation: AtomicU64,
    /// Set until the form's first load opens the saved server; Android drops a navigation
    /// that starts before the first page has loaded.
    reopen: AtomicBool,
    /// A notification route that arrived before the server page was open.
    pending_route: Mutex<Option<String>>,
    push: push::Push,
}

fn lock<T>(value: &Mutex<T>) -> Result<MutexGuard<'_, T>> {
    value
        .lock()
        .map_err(|_| "Phone state lock poisoned.".into())
}

/// The server origins a phone accepts: HTTPS. Debug builds also accept plain HTTP on
/// loopback, which `adb reverse` maps to the development host. A Minarbot server answers
/// only on its own address, so the emulator's `10.0.2.2` alias would be refused anyway.
pub(crate) fn address(input: &str) -> Result<Url> {
    let url = origin(input)?;
    if url.scheme() != "https" && !cfg!(debug_assertions) {
        return Err("Use the server's HTTPS address.".into());
    }
    Ok(url)
}

fn connect_page(url: &Url) -> bool {
    matches!(
        (url.scheme(), url.host_str()),
        ("tauri", Some("localhost")) | ("http" | "https", Some("tauri.localhost"))
    ) && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && matches!(url.path(), "/" | "/index.html")
}

fn window(app: &AppHandle) -> Result<WebviewWindow> {
    app.get_webview_window(WINDOW)
        .ok_or_else(|| "The app window is missing.".into())
}

fn showing_server(app: &AppHandle) -> Result<bool> {
    let current = window(app)?.url()?;
    Ok(lock(&app.state::<Phone>().server)?
        .as_ref()
        .is_some_and(|server| same_origin(&current, server)))
}

/// Shows the bundled form with a status or error, and the saved server filled in.
fn show_connect(app: &AppHandle, status: &str, error: &str) -> Result<()> {
    let state = app.state::<Phone>();
    *lock(&state.form)? = (status.to_owned(), error.to_owned());
    let window = window(app)?;
    let current = window.url()?;
    if connect_page(&current) {
        return fill_form(app, &window);
    }
    *lock(&state.server)? = None;
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("status", status)
        .append_pair("error", error)
        .finish();
    // The page-load handler fills in the address once the form has loaded.
    window.navigate(connect_origin(&current).join(&format!("index.html?{query}"))?)?;
    Ok(())
}

/// The shared form reads its status from the query; the saved address comes from here.
fn fill_form(app: &AppHandle, window: &WebviewWindow) -> Result<()> {
    let state = app.state::<Phone>();
    let saved = lock(&state.saved)?
        .as_ref()
        .map(Url::to_string)
        .unwrap_or_default();
    let (status, error) = lock(&state.form)?.clone();
    window.eval(format!(
        "(() => {{ const update = () => {{
            const input = document.querySelector('#server-url');
            if (input && !input.value) input.value = {saved};
            document.querySelector('#status').textContent = {status};
            document.querySelector('#error').textContent = {error};
        }};
        if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', update, {{ once: true }});
        else update(); }})();",
        saved = serde_json::to_string(&saved)?,
        status = serde_json::to_string(&status)?,
        error = serde_json::to_string(&error)?,
    ))?;
    Ok(())
}

/// The bundled page's origin differs per platform (tauri://localhost, http://tauri.localhost).
fn connect_origin(current: &Url) -> Url {
    if connect_page(current) {
        return current.clone();
    }
    let fallback = if cfg!(any(target_os = "android", windows)) {
        "http://tauri.localhost/"
    } else {
        "tauri://localhost/"
    };
    Url::parse(fallback).unwrap_or_else(|_| current.clone())
}

fn show_server(app: &AppHandle, url: Url, generation: u64) -> Result<()> {
    let state = app.state::<Phone>();
    if state.generation.load(Ordering::SeqCst) != generation {
        return Ok(());
    }
    Settings {
        server: Some(url.to_string()),
    }
    .save(&state.path)?;
    *lock(&state.saved)? = Some(url.clone());
    *lock(&state.server)? = Some(url.clone());
    window(app)?.navigate(url)?;
    Ok(())
}

fn next_generation(app: &AppHandle) -> u64 {
    app.state::<Phone>()
        .generation
        .fetch_add(1, Ordering::SeqCst)
        + 1
}

/// Probes the server, then opens its web client; on failure shows the form with the error.
fn start_connection(app: &AppHandle, url: Url) {
    let app = app.clone();
    let generation = next_generation(&app);
    log::info!("Connecting to {}", url.origin().ascii_serialization());
    tauri::async_runtime::spawn(async move {
        let result = match probe(&url).await {
            Ok(()) => show_server(&app, url, generation),
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            if app.state::<Phone>().generation.load(Ordering::SeqCst) == generation {
                log::warn!("Connection failed: {error}");
                if let Err(error) = show_connect(&app, "", &error.to_string()) {
                    log::error!("Could not show the connect form: {error}");
                }
            }
        }
    });
}

#[tauri::command]
async fn connect_server(
    address: String,
    webview: WebviewWindow,
) -> std::result::Result<(), String> {
    if webview.label() != WINDOW || !webview.url().is_ok_and(|url| connect_page(&url)) {
        return Err("Untrusted connection window.".into());
    }
    let app = webview.app_handle();
    let generation = next_generation(app);
    let url = self::address(&address).map_err(|error| error.to_string())?;
    probe(&url).await.map_err(|error| error.to_string())?;
    show_server(app, url, generation).map_err(|error| error.to_string())
}

/// Coming back to the app: check the server is still there, and let the web client catch up.
fn resumed(app: &AppHandle) {
    log::info!("Resumed");
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = async {
            if showing_server(&app)? {
                let server = lock(&app.state::<Phone>().server)?.clone();
                if let Some(server) = server {
                    if let Err(error) = probe(&server).await {
                        log::warn!("Server unreachable after resume: {error}");
                        next_generation(&app);
                        show_connect(&app, "", &error.to_string())?;
                    } else {
                        window(&app)?
                            .eval("document.dispatchEvent(new Event('visibilitychange'))")?;
                    }
                }
            } else {
                // After a failed attempt, try the saved server again. Bind before acting:
                // an `if let` would hold the lock while `show_connect` takes it again.
                let failed = !lock(&app.state::<Phone>().form)?.1.is_empty();
                let saved = lock(&app.state::<Phone>().saved)?.clone();
                if let (true, Some(saved)) = (failed, saved) {
                    show_connect(&app, "Connecting…", "")?;
                    start_connection(&app, saved);
                }
            }
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        }
        .await;
        if let Err(error) = result {
            log::error!("Resume failed: {error}");
        }
    });
}

/// The server route a notification asks to open, such as `/agents/<id>`.
fn route(server: &Url, path: &str) -> Option<Url> {
    if !path.starts_with('/') || path.starts_with("//") || path.contains('\\') {
        return None;
    }
    server
        .join(path)
        .ok()
        .filter(|url| same_origin(url, server))
}

/// Opens a notification's route in the server page, or once the server page is open.
pub(crate) fn open_route(app: &AppHandle, path: &str) {
    let result = (|| -> Result<()> {
        let state = app.state::<Phone>();
        if !showing_server(app)? {
            *lock(&state.pending_route)? = Some(path.to_owned());
            return Ok(());
        }
        let server = lock(&state.server)?.clone();
        let Some(target) = server.as_ref().and_then(|server| route(server, path)) else {
            log::warn!("Push: ignored a notification route outside the server");
            return Ok(());
        };
        window(app)?.navigate(target)?;
        Ok(())
    })();
    if let Err(error) = result {
        log::error!("Push: could not open the notification: {error}");
    }
}

fn navigation(app: &AppHandle, target: &Url) -> bool {
    if connect_page(target) {
        return true;
    }
    let allowed = app
        .try_state::<Phone>()
        .and_then(|state| state.server.lock().ok().map(|server| server.clone()))
        .flatten()
        .is_some_and(|server| same_origin(target, &server));
    if !allowed && external(target) {
        if let Err(error) = app.opener().open_url(target.as_str(), None::<&str>) {
            log::warn!("Could not open {target}: {error}");
        }
    }
    allowed
}

fn page_loaded(window: &WebviewWindow, url: &Url) {
    let app = window.app_handle();
    native::match_system_bars(window);
    if connect_page(url) {
        if let Err(error) = fill_form(app, window) {
            log::error!("Could not fill the connect form: {error}");
        }
        let state = app.state::<Phone>();
        if state.reopen.swap(false, Ordering::SeqCst) {
            let saved = state.saved.lock().ok().and_then(|saved| saved.clone());
            if let Some(saved) = saved {
                start_connection(app, saved);
            }
        }
        return;
    }
    let Some(server) = app
        .try_state::<Phone>()
        .and_then(|state| state.server.lock().ok().map(|server| server.clone()))
        .flatten()
    else {
        return;
    };
    if same_origin(url, &server) {
        log::info!("Opened {}", server.origin().ascii_serialization());
        let state = app.state::<Phone>();
        state.push.deliver(window, &server);
        if let Some(path) = state
            .pending_route
            .lock()
            .ok()
            .and_then(|mut path| path.take())
        {
            open_route(app, &path);
        }
    }
}

fn setup(app: &mut tauri::App) -> Result<()> {
    let path = app.path().app_config_dir()?.join("settings.json");
    let saved = Settings::load(&path).server();
    app.manage(Phone {
        path,
        server: Mutex::new(None),
        saved: Mutex::new(saved.clone()),
        form: Mutex::default(),
        generation: AtomicU64::new(0),
        reopen: AtomicBool::new(saved.is_some()),
        pending_route: Mutex::default(),
        push: push::Push::default(),
    });
    let status = if saved.is_some() { "Connecting…" } else { "" };
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("status", status)
        .finish();
    let handle = app.handle().clone();
    WebviewWindowBuilder::new(
        app,
        WINDOW,
        WebviewUrl::App(format!("index.html?{query}").into()),
    )
    .on_navigation(move |target| navigation(&handle, target))
    .on_page_load(|window, payload| {
        if payload.event() == PageLoadEvent::Finished {
            page_loaded(&window, payload.url());
        }
    })
    .build()?;
    push::register(app.handle());
    native::on_resume(app.handle(), resumed);
    *lock(&app.state::<Phone>().form)? = (status.into(), String::new());
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .build(),
        )
        .plugin(
            tauri_plugin_opener::Builder::new()
                .open_js_links_on_click(false)
                .build(),
        )
        .plugin(native::init())
        .invoke_handler(tauri::generate_handler![connect_server])
        .setup(|app| {
            if let Err(error) = setup(app) {
                log::error!("Minarbot could not start: {error}");
                return Err(error);
            }
            Ok(())
        })
        .build(tauri::generate_context!());
    match app {
        Ok(app) => app.run(|_, _| {}),
        Err(error) => log::error!("Minarbot failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_addresses_are_https_except_debug_loopback() -> Result<()> {
        assert_eq!(
            address("https://server.example:4317")?.as_str(),
            "https://server.example:4317/"
        );
        for invalid in [
            "http://server.example:4317",
            "http://10.0.2.2:4339",
            "http://127.0.0.1:4339/path",
        ] {
            assert!(address(invalid).is_err(), "{invalid}");
        }
        assert_eq!(
            address("http://127.0.0.1:4339").is_ok(),
            cfg!(debug_assertions)
        );
        Ok(())
    }

    #[test]
    fn notification_routes_stay_on_the_server() -> Result<()> {
        let server = Url::parse("https://server.example:4317/")?;
        assert_eq!(
            route(&server, "/agents/a1/conversations/t1").map(String::from),
            Some("https://server.example:4317/agents/a1/conversations/t1".into())
        );
        for path in [
            "agents",
            "//other.example/",
            "/\\other.example",
            "https://other.example/",
        ] {
            assert!(route(&server, path).is_none(), "{path}");
        }
        Ok(())
    }

    #[test]
    fn only_the_bundled_page_has_ipc_permissions() -> Result<()> {
        for page in [
            "tauri://localhost/index.html?status=x",
            "http://tauri.localhost/",
            "https://tauri.localhost/index.html",
        ] {
            assert!(connect_page(&Url::parse(page)?), "{page}");
        }
        for page in [
            "http://10.0.2.2:4339/",
            "https://server.example/",
            "http://tauri.localhost:8080/",
            "http://tauri.localhost/other.html",
        ] {
            assert!(!connect_page(&Url::parse(page)?), "{page}");
        }
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/connect.json"))?;
        assert_eq!(capability["webviews"], serde_json::json!([WINDOW]));
        assert_eq!(
            capability["permissions"],
            serde_json::json!(["allow-connect-server"])
        );
        // A capability without `remote` applies only to the bundled origin, never the server page.
        assert!(capability.get("remote").is_none());
        let config: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json"))?;
        assert_eq!(
            config["app"]["security"]["capabilities"],
            serde_json::json!(["connect"])
        );
        Ok(())
    }
}
