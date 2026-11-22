#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(target_os = "linux")]
mod clipboard;
mod connection;
mod local;
mod settings;
mod updates;

use connection::{external, origin, probe, same_origin, Result};
use settings::{Bounds, Settings};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, MutexGuard,
    },
};
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    webview::{NewWindowResponse, WebviewWindowBuilder},
    AppHandle, Manager, Url, WebviewUrl, WebviewWindow, WindowEvent, Wry,
};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

struct Desktop {
    settings: Mutex<Settings>,
    path: PathBuf,
    visible: Mutex<String>,
    generation: AtomicU64,
    zoom: Mutex<f64>,
    local: local::Local,
    updates: updates::Updates,
}

fn lock<T>(value: &Mutex<T>) -> Result<MutexGuard<'_, T>> {
    value
        .lock()
        .map_err(|_| "Desktop state lock poisoned.".into())
}

fn current(app: &AppHandle) -> Option<WebviewWindow> {
    let state = app.try_state::<Desktop>()?;
    let label = lock(&state.visible).ok()?.clone();
    app.get_webview_window(&label)
}

fn focus(window: &WebviewWindow) -> Result<()> {
    window.show()?;
    window.unminimize()?;
    window.set_focus()?;
    Ok(())
}

fn report(app: &AppHandle, message: impl Into<String>) {
    let dialog = app.dialog().message(message).title("Minarbot");
    let dialog = if let Some(window) = current(app) {
        dialog.parent(&window)
    } else {
        dialog
    };
    dialog.show(|_| {});
}

fn open_external(url: Url) {
    if external(&url) {
        let _ = tauri_plugin_opener::open_url(url.as_str(), None::<&str>);
    }
}

fn saved_bounds(window: &WebviewWindow) -> Option<Bounds> {
    if window.is_minimized().ok()? || window.is_maximized().ok()? || window.is_fullscreen().ok()? {
        return None;
    }
    let scale = window.scale_factor().ok()?;
    let position = window.outer_position().ok()?.to_logical::<i32>(scale);
    let size = window.inner_size().ok()?.to_logical::<u32>(scale);
    Some(Bounds {
        x: Some(position.x),
        y: Some(position.y),
        width: size.width,
        height: size.height,
    })
}

fn fit(bounds: Bounds, display: &tauri::Monitor) -> bool {
    let (Some(x), Some(y)) = (bounds.x, bounds.y) else {
        return false;
    };
    let scale = display.scale_factor();
    let area = display.work_area();
    let point = area.position.to_logical::<i32>(scale);
    let size = area.size.to_logical::<i32>(scale);
    let (x, y) = (i64::from(x), i64::from(y));
    x >= i64::from(point.x) - 1
        && y >= i64::from(point.y) - 1
        && x + i64::from(bounds.width) <= i64::from(point.x) + i64::from(size.width) + 1
        && y + i64::from(bounds.height) <= i64::from(point.y) + i64::from(size.height) + 1
}

fn window<'a>(
    app: &'a AppHandle,
    label: &str,
    url: WebviewUrl,
    bounds: Option<Bounds>,
) -> Result<WebviewWindowBuilder<'a, Wry, AppHandle>> {
    let mut builder = WebviewWindowBuilder::new(app, label, url)
        .title("Minarbot")
        .visible(false)
        .min_inner_size(640.0, 480.0)
        .inner_size(1200.0, 800.0)
        .devtools(false);
    if let Some(bounds) = bounds {
        let displays = app.available_monitors()?;
        if displays.iter().any(|display| fit(bounds, display)) {
            if let (Some(x), Some(y)) = (bounds.x, bounds.y) {
                builder = builder
                    .position(x.into(), y.into())
                    .inner_size(bounds.width.into(), bounds.height.into());
            }
        }
    }
    Ok(builder)
}

fn switch_window(app: &AppHandle, next: WebviewWindow) -> Result<()> {
    let previous = current(app);
    focus(&next)?;
    *lock(&app.state::<Desktop>().visible)? = next.label().to_owned();
    if let Some(previous) = previous.filter(|previous| previous.label() != next.label()) {
        previous.destroy()?;
    }
    Ok(())
}

fn connect_status(app: &AppHandle, status: &str, error: &str) -> Result<()> {
    if let Some(connect) = app.get_webview_window("connect") {
        connect.eval(format!(
            "(() => {{ const update = () => {{
                document.querySelector('#status').textContent = {};
                document.querySelector('#error').textContent = {};
            }};
            if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', update, {{ once: true }});
            else update(); }})();",
            serde_json::to_string(status)?, serde_json::to_string(error)?,
        ))?;
    }
    Ok(())
}

fn show_connect(app: &AppHandle, status: &str) -> Result<()> {
    if let Some(connect) = app.get_webview_window("connect") {
        focus(&connect)?;
        return connect_status(app, status, "");
    }
    let state = app.state::<Desktop>();
    let bounds = current(app)
        .as_ref()
        .and_then(saved_bounds)
        .or(lock(&state.settings)?.window);
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("status", status)
        .finish();
    let next = window(
        app,
        "connect",
        WebviewUrl::App(format!("index.html?{query}").into()),
        bounds,
    )?
    .on_navigation(|url| {
        (url.scheme() == "tauri" && url.host_str() == Some("localhost")
            || url.scheme() == "http" && url.host_str() == Some("tauri.localhost"))
            && matches!(url.path(), "/" | "/index.html")
    })
    .on_new_window(|_, _| NewWindowResponse::Deny)
    .build()?;
    switch_window(app, next)
}

fn show_server(app: &AppHandle, url: Url, generation: u64, managed_local: bool) -> Result<()> {
    let state = app.state::<Desktop>();
    if state.generation.load(Ordering::SeqCst) != generation {
        return Ok(());
    }
    let mut settings = lock(&state.settings)?.clone();
    settings.window = current(app)
        .as_ref()
        .and_then(saved_bounds)
        .or(settings.window);
    settings.select(&url, managed_local);
    let configured = url.clone();
    #[cfg(target_os = "linux")]
    let notification_origin = url.clone();
    let data = app.path().app_data_dir()?.join("webview");
    let next = window(
        app,
        &format!("server-{generation}"),
        WebviewUrl::External(url),
        settings.window,
    )?
    .data_directory(data)
    .zoom_hotkeys_enabled(true)
    .initialization_script_for_all_frames(include_str!("top-level.js"))
    .on_navigation(move |target| same_origin(target, &configured))
    .on_new_window(|target, _features| {
        open_external(target);
        NewWindowResponse::Deny
    })
    .build()?;
    #[cfg(target_os = "linux")]
    {
        clipboard::install(&next, &notification_origin)?;
        linux_notifications(&next, notification_origin)?;
    }
    if let Err(error) = settings.save(&state.path) {
        let _ = next.destroy();
        return Err(error);
    }
    *lock(&state.settings)? = settings;
    refresh_menu(app)?;
    switch_window(app, next)
}

#[cfg(target_os = "linux")]
fn linux_notifications(window: &WebviewWindow, configured: Url) -> Result<()> {
    use webkit2gtk::{
        glib::object::Cast, NotificationPermissionRequest, PermissionRequestExt, WebViewExt,
    };
    let view = window.clone();
    window.with_webview(move |native| {
        native
            .inner()
            .connect_permission_request(move |webview, request| {
                if request
                    .downcast_ref::<NotificationPermissionRequest>()
                    .is_none()
                {
                    request.deny();
                    return true;
                }
                let at_origin = webview
                    .uri()
                    .and_then(|uri| Url::parse(&uri).ok())
                    .is_some_and(|url| same_origin(&url, &configured));
                if !at_origin || !view.is_focused().unwrap_or(false) {
                    request.deny();
                    return true;
                }
                // The browser owns secure-context checks. Grant permission only with native consent.
                let request = request.clone();
                let (send, receive) = tokio::sync::oneshot::channel();
                view.app_handle()
                    .dialog()
                    .message("Allow this server to send notifications?")
                    .title("Minarbot notifications")
                    .parent(&view)
                    .buttons(MessageDialogButtons::OkCancelCustom(
                        "Allow".into(),
                        "Deny".into(),
                    ))
                    .show(move |allow| {
                        let _ = send.send(allow);
                    });
                let view = view.clone();
                webkit2gtk::glib::MainContext::default().spawn_local(async move {
                    if receive.await.unwrap_or(false) && view.is_visible().unwrap_or(false) {
                        request.allow();
                    } else {
                        request.deny();
                    }
                });
                true
            });
    })?;
    Ok(())
}

async fn on_main<T: Send + 'static>(
    app: &AppHandle,
    f: impl FnOnce(&AppHandle) -> Result<T> + Send + 'static,
) -> Result<T> {
    let handle = app.clone();
    let (send, receive) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let _ = send.send(f(&handle));
    })?;
    receive.await?
}

async fn select(app: &AppHandle, url: Url, generation: u64, managed: bool) -> Result<()> {
    probe(&url).await?;
    on_main(app, move |app| show_server(app, url, generation, managed)).await
}

#[tauri::command]
async fn connect_server(
    address: String,
    webview: WebviewWindow,
) -> std::result::Result<(), String> {
    if webview.label() != "connect" {
        return Err("Untrusted connection window.".into());
    }
    let app = webview.app_handle();
    let generation = on_main(app, |app| {
        Ok(app
            .state::<Desktop>()
            .generation
            .fetch_add(1, Ordering::SeqCst)
            + 1)
    })
    .await
    .map_err(|error| error.to_string())?;
    let url = origin(&address).map_err(|error| error.to_string())?;
    select(app, url, generation, false)
        .await
        .map_err(|error| error.to_string())
}

fn start_connection(app: &AppHandle, address: Option<String>) {
    let app = app.clone();
    let generation = app
        .state::<Desktop>()
        .generation
        .fetch_add(1, Ordering::SeqCst)
        + 1;
    tauri::async_runtime::spawn(async move {
        let result = async {
            let address = address.as_deref().map(origin).transpose()?;
            let (url, managed) = match address {
                Some(url) if !local::Local::is_managed(&url) => (url, false),
                _ => (app.state::<Desktop>().local.start(&app).await?, true),
            };
            select(&app, url, generation, managed).await
        }
        .await;
        if let Err(error) = result {
            let message = error.to_string();
            let _ = on_main(&app, move |app| {
                if app.state::<Desktop>().generation.load(Ordering::SeqCst) != generation {
                    return Ok(());
                }
                show_connect(app, "")?;
                connect_status(app, "", &message)
            })
            .await;
        }
    });
}

fn refresh_menu(app: &AppHandle) -> Result<()> {
    let settings = lock(&app.state::<Desktop>().settings)?.clone();
    let about = MenuItem::with_id(app, "about", "About Minarbot", true, None::<&str>)?;
    let updates = MenuItem::with_id(
        app,
        "updates",
        "Check for Updates…",
        true,
        Some("CmdOrCtrl+Shift+F5"),
    )?;
    let quit = PredefinedMenuItem::quit(app, Some("Quit Minarbot"))?;
    let application = Submenu::with_items(app, "Minarbot", true, &[&about, &updates, &quit])?;
    let servers = Submenu::new(app, "Servers", true)?;
    servers.append(&MenuItem::with_id(
        app,
        "connect",
        "Connect to Server…",
        true,
        None::<&str>,
    )?)?;
    servers.append(&MenuItem::with_id(
        app,
        "local",
        if cfg!(windows) {
            "Start WSL Server"
        } else {
            "Start Local Server"
        },
        true,
        None::<&str>,
    )?)?;
    let remove = Submenu::new(app, "Remove Server", !settings.servers.is_empty())?;
    for address in &settings.servers {
        servers.append(&CheckMenuItem::with_id(
            app,
            format!("select:{address}"),
            address,
            true,
            settings.active_server.as_ref() == Some(address),
            None::<&str>,
        )?)?;
        remove.append(&MenuItem::with_id(
            app,
            format!("remove:{address}"),
            address,
            true,
            None::<&str>,
        )?)?;
    }
    servers.append(&remove)?;
    #[cfg(windows)]
    servers.append(&MenuItem::with_id(
        app,
        "wsl-help",
        "WSL Setup Guide",
        true,
        None::<&str>,
    )?)?;
    #[cfg(target_os = "linux")]
    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &MenuItem::with_id(app, "edit:Undo", "Undo", true, None::<&str>)?,
            &MenuItem::with_id(app, "edit:Redo", "Redo", true, None::<&str>)?,
            &MenuItem::with_id(app, "edit:Cut", "Cut", true, None::<&str>)?,
            &MenuItem::with_id(app, "edit:Copy", "Copy", true, None::<&str>)?,
            &MenuItem::with_id(app, "edit:Paste", "Paste", true, None::<&str>)?,
            &MenuItem::with_id(app, "edit:SelectAll", "Select All", true, None::<&str>)?,
        ],
    )?;
    #[cfg(not(target_os = "linux"))]
    let edit = {
        #[cfg(target_os = "macos")]
        let (undo, redo) = (
            PredefinedMenuItem::undo(app, None)?,
            PredefinedMenuItem::redo(app, None)?,
        );
        #[cfg(windows)]
        let (undo, redo) = (
            MenuItem::with_id(app, "edit:Undo", "Undo", true, None::<&str>)?,
            MenuItem::with_id(app, "edit:Redo", "Redo", true, None::<&str>)?,
        );
        Submenu::with_items(
            app,
            "Edit",
            true,
            &[
                &undo,
                &redo,
                &PredefinedMenuItem::separator(app)?,
                &PredefinedMenuItem::cut(app, None)?,
                &PredefinedMenuItem::copy(app, None)?,
                &PredefinedMenuItem::paste(app, None)?,
                &PredefinedMenuItem::select_all(app, None)?,
            ],
        )?
    };
    let view = Submenu::with_items(
        app,
        "View",
        true,
        &[
            &MenuItem::with_id(app, "reload", "Reload", true, Some("CmdOrCtrl+R"))?,
            &MenuItem::with_id(app, "zoom:0", "Reset Zoom", true, Some("CmdOrCtrl+0"))?,
            &MenuItem::with_id(app, "zoom:+", "Zoom In", true, None::<&str>)?,
            &MenuItem::with_id(app, "zoom:-", "Zoom Out", true, None::<&str>)?,
            &MenuItem::with_id(app, "fullscreen", "Toggle Full Screen", true, Some("F11"))?,
        ],
    )?;
    app.set_menu(Menu::with_items(
        app,
        &[&application, &servers, &edit, &view],
    )?)?;
    Ok(())
}

fn remove_server(app: &AppHandle, address: String) {
    let handle = app.clone();
    app.dialog()
        .message(format!("Remove {address} from saved servers?"))
        .title("Minarbot")
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Remove".into(),
            "Cancel".into(),
        ))
        .show(move |remove| {
            if !remove {
                return;
            }
            let app = handle.clone();
            let _ = handle.run_on_main_thread(move || {
                let result = (|| -> Result<()> {
                    let state = app.state::<Desktop>();
                    let mut settings = lock(&state.settings)?.clone();
                    let was_active = settings.active_server.as_deref() == Some(&address);
                    settings.remove(&address);
                    settings.save(&state.path)?;
                    *lock(&state.settings)? = settings.clone();
                    refresh_menu(&app)?;
                    if was_active {
                        state.generation.fetch_add(1, Ordering::SeqCst);
                        show_connect(&app, "")?;
                        if let Some(next) = settings.active_server {
                            start_connection(&app, Some(next));
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    report(&app, error.to_string());
                }
            });
        });
}

fn menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) -> Result<()> {
    let state = app.state::<Desktop>();
    let id = event.id().as_ref();
    match id {
        "about" => report(
            app,
            format!("Minarbot Desktop {}", app.package_info().version),
        ),
        "updates" => updates::check(app, true),
        #[cfg(windows)]
        "wsl-help" => open_external(Url::parse(
            "https://learn.microsoft.com/windows/wsl/install",
        )?),
        "connect" => {
            state.generation.fetch_add(1, Ordering::SeqCst);
            show_connect(app, "")?;
        }
        "local" => {
            show_connect(app, "Starting Minarbot…")?;
            start_connection(app, None);
        }
        _ if id.starts_with("select:") => start_connection(app, Some(id[7..].to_owned())),
        _ if id.starts_with("remove:") => remove_server(app, id[7..].to_owned()),
        _ => {
            let Some(window) = current(app) else {
                return Ok(());
            };
            match id {
                "reload" => window.reload()?,
                "fullscreen" => window.set_fullscreen(!window.is_fullscreen()?)?,
                "zoom:0" | "zoom:+" | "zoom:-" => {
                    let mut zoom = lock(&state.zoom)?;
                    *zoom = match id {
                        "zoom:+" => (*zoom * 1.1).min(3.0),
                        "zoom:-" => (*zoom / 1.1).max(0.5),
                        _ => 1.0,
                    };
                    window.set_zoom(*zoom)?;
                }
                #[cfg(windows)]
                "edit:Undo" => window.eval("document.execCommand('undo')")?,
                #[cfg(windows)]
                "edit:Redo" => window.eval("document.execCommand('redo')")?,
                #[cfg(target_os = "linux")]
                "edit:Undo" | "edit:Redo" | "edit:Cut" | "edit:Copy" | "edit:Paste"
                | "edit:SelectAll" => {
                    let command = id[5..].to_owned();
                    window.with_webview(move |view| {
                        use webkit2gtk::WebViewExt;
                        view.inner().execute_editing_command(&command);
                    })?;
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn electron_settings(app: &tauri::App) -> Result<PathBuf> {
    #[cfg(target_os = "linux")]
    let directory = app.path().config_dir()?;
    #[cfg(not(target_os = "linux"))]
    let directory = app.path().data_dir()?;
    Ok(directory.join("minarbot-desktop/settings.json"))
}

fn setup(app: &mut tauri::App) -> Result<()> {
    if option_env!("MINARBOT_DESKTOP_FIXTURE_PORT").is_some()
        && std::env::var_os("HOME")
            .is_none_or(|home| Some(home) != std::env::var_os("MINARBOT_DESKTOP_FIXTURE_HOME"))
    {
        return Err("Fixture builds require a disposable HOME.".into());
    }
    let path = app.path().app_config_dir()?.join("settings.json");
    let settings = Settings::load(&path, &electron_settings(app)?)?;
    let managed = settings.managed_local;
    let address = if managed {
        None
    } else {
        settings.active_server.clone()
    };
    app.manage(Desktop {
        settings: Mutex::new(settings),
        path,
        visible: Mutex::new(String::new()),
        generation: AtomicU64::new(0),
        zoom: Mutex::new(1.0),
        local: local::Local::default(),
        updates: updates::Updates::default(),
    });
    refresh_menu(app.handle())?;
    show_connect(
        app.handle(),
        if address.is_none() {
            "Starting Minarbot…"
        } else {
            "Connecting…"
        },
    )?;
    start_connection(app.handle(), address);
    let app = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(8)).await;
            updates::check(&app, false);
            tokio::time::sleep(std::time::Duration::from_secs(6 * 3600 - 8)).await;
        }
    });
    Ok(())
}

fn persist_bounds(app: &AppHandle) -> Result<()> {
    let state = app.state::<Desktop>();
    if let Some(bounds) = current(app).as_ref().and_then(saved_bounds) {
        let mut settings = lock(&state.settings)?.clone();
        settings.window = Some(bounds);
        settings.save(&state.path)?;
        *lock(&state.settings)? = settings;
    }
    Ok(())
}

fn main() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = current(app) {
                let _ = focus(&window);
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_opener::Builder::new()
                .open_js_links_on_click(false)
                .build(),
        )
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![connect_server])
        .setup(|app| {
            if let Err(error) = setup(app) {
                let handle = app.handle().clone();
                app.dialog()
                    .message(error.to_string())
                    .title("Minarbot")
                    .show(move |_| handle.exit(1));
            }
            Ok(())
        })
        .on_menu_event(|app, event| {
            if let Err(error) = menu_event(app, event) {
                report(app, error.to_string());
            }
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                if let Err(error) = persist_bounds(app) {
                    report(app, error.to_string());
                }
                #[cfg(target_os = "macos")]
                {
                    api.prevent_close();
                    let _ = window.hide();
                }
                #[cfg(not(target_os = "macos"))]
                {
                    api.prevent_close();
                    app.exit(0);
                }
            }
        })
        .build(tauri::generate_context!());
    match app {
        Ok(app) => app.run(|app, event| match event {
            tauri::RunEvent::Exit => {
                if let Some(state) = app.try_state::<Desktop>() {
                    let _ = persist_bounds(app);
                    state.local.stop();
                }
            }
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen {
                has_visible_windows: false,
                ..
            } => {
                if let Some(window) = current(app) {
                    let _ = focus(&window);
                }
            }
            _ => {}
        }),
        Err(error) => {
            eprintln!("Minarbot desktop failed: {error}");
            std::process::exit(1);
        }
    }
}
