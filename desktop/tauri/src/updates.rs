use crate::connection::Result;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_updater::{Update, UpdaterExt};

#[derive(Default)]
pub struct Updates {
    busy: tokio::sync::Mutex<()>,
    ready: tokio::sync::Mutex<Option<(Update, Vec<u8>)>>,
}

pub fn check(app: &AppHandle, manual: bool) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<crate::Desktop>();
        let Ok(_busy) = state.updates.busy.try_lock() else {
            return;
        };
        if check_update(&app, &state.updates, manual).await.is_err() {
            eprintln!("Minarbot desktop update failed. Retry from the app menu.");
            if manual {
                super::report(
                    &app,
                    "Could not verify or install the update. Try again from the Minarbot menu.",
                );
            }
        }
    });
}

async fn check_update(app: &AppHandle, state: &Updates, manual: bool) -> Result<()> {
    #[cfg(target_os = "linux")]
    if std::env::var_os("APPIMAGE").is_none() {
        if manual {
            super::report(app, "Updates require an installed AppImage.");
        }
        return Ok(());
    }
    if app
        .config()
        .plugins
        .0
        .get("updater")
        .and_then(|config| config["endpoints"].as_array())
        .is_none_or(Vec::is_empty)
    {
        if manual {
            super::report(app, "This build has no update feed.");
        }
        return Ok(());
    }
    let mut ready = state.ready.lock().await;
    if ready.is_some() && !manual {
        return Ok(());
    }
    if ready.is_none() {
        let updater = app
            .updater_builder()
            .timeout(Duration::from_secs(10))
            .build()?;
        let Some(update) = updater.check().await? else {
            if manual {
                super::report(app, "Minarbot Desktop is up to date.");
            }
            return Ok(());
        };
        let bytes = download(&update).await?;
        *ready = Some((update, bytes));
    }
    let Some((update, bytes)) = ready.as_ref() else {
        return Ok(());
    };
    let (action, message) = if cfg!(target_os = "macos") {
        ("Open Disk Image", format!("Minarbot {} is verified. Quit Minarbot and replace it in Applications from the disk image.", update.version))
    } else {
        (
            "Restart to Update",
            format!("Minarbot {} is ready to install.", update.version),
        )
    };
    let (send, receive) = tokio::sync::oneshot::channel();
    let dialog = app.dialog().message(message).title("Update Ready").buttons(
        MessageDialogButtons::OkCancelCustom(action.into(), "Later".into()),
    );
    let dialog = if let Some(window) = super::current(app) {
        dialog.parent(&window)
    } else {
        dialog
    };
    dialog.show(move |accepted| {
        let _ = send.send(accepted);
    });
    if !receive.await? {
        return Ok(());
    }
    update.verify(bytes)?;
    #[cfg(target_os = "linux")]
    {
        install_appimage(bytes)?;
        app.request_restart();
    }
    #[cfg(windows)]
    update.install(bytes)?;
    #[cfg(target_os = "macos")]
    open_dmg(app, &update.version, bytes)?;
    Ok(())
}

fn allowed_download(url: &url::Url) -> bool {
    let github = url.scheme() == "https"
        && url.port().is_none()
        && matches!(
            url.host_str(),
            Some(
                "github.com"
                    | "objects.githubusercontent.com"
                    | "release-assets.githubusercontent.com"
            )
        );
    let fixture = option_env!("MINARBOT_DESKTOP_FIXTURE_PORT").is_some()
        && url.scheme() == "http"
        && crate::connection::loopback(url);
    (github || fixture) && url.username().is_empty() && url.password().is_none()
}

async fn download(update: &Update) -> Result<Vec<u8>> {
    const LIMIT: usize = 1024 * 1024 * 1024;
    if !allowed_download(&update.download_url) {
        return Err("Invalid desktop update URL.".into());
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() < 4 && allowed_download(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("Invalid desktop update redirect")
            }
        }))
        .timeout(Duration::from_secs(300))
        .build()?;
    let mut response = client
        .get(update.download_url.clone())
        .send()
        .await?
        .error_for_status()?;
    if response
        .content_length()
        .is_some_and(|size| size > LIMIT as u64)
    {
        return Err("Desktop update exceeds 1 GiB.".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > LIMIT {
            return Err("Desktop update exceeds 1 GiB.".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    update.verify(&bytes)?;
    Ok(bytes)
}

#[cfg(target_os = "linux")]
fn install_appimage(bytes: &[u8]) -> Result<()> {
    use std::{
        fs,
        io::{self, Write},
        path::PathBuf,
    };
    if !bytes.starts_with(b"\x7fELF") || bytes.get(8..11) != Some(b"AI\x02") {
        return Err("Update is not a type 2 AppImage.".into());
    }
    let path = fs::canonicalize(PathBuf::from(
        std::env::var_os("APPIMAGE").ok_or("Missing AppImage path.")?,
    ))?;
    let directory = path
        .parent()
        .ok_or("Missing AppImage directory.")?
        .to_path_buf();
    let stem = path.file_stem().ok_or("Invalid AppImage filename.")?;
    let name = format!("{}.previous.AppImage", stem.to_string_lossy());
    let mut original = fs::File::open(&path)?;
    let permissions = original.metadata()?.permissions();
    let mut replacement = tempfile::NamedTempFile::new_in(&directory)?;
    replacement.write_all(bytes)?;
    replacement.as_file().set_permissions(permissions.clone())?;
    replacement.as_file().sync_all()?;
    let mut previous = tempfile::NamedTempFile::new_in(&directory)?;
    io::copy(&mut original, &mut previous)?;
    previous.as_file().set_permissions(permissions)?;
    previous.as_file().sync_all()?;
    previous
        .persist(directory.join(name))
        .map_err(|error| error.error)?;
    replacement.persist(&path).map_err(|error| error.error)?;
    fs::File::open(&directory)?.sync_all()?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn open_dmg(app: &AppHandle, version: &str, bytes: &[u8]) -> Result<()> {
    use std::{fs, io::Write};
    use tauri_plugin_opener::OpenerExt;
    let directory = app.path().app_cache_dir()?.join("updates");
    fs::create_dir_all(&directory)?;
    let file = directory.join(format!("Minarbot-Tauri-{version}.dmg"));
    let mut stage = tempfile::NamedTempFile::new_in(&directory)?;
    stage.write_all(bytes)?;
    stage.as_file().sync_all()?;
    stage.persist(&file).map_err(|error| error.error)?;
    app.opener()
        .open_path(file.to_string_lossy(), None::<&str>)?;
    Ok(())
}
