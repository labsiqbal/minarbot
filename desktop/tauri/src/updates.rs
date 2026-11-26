use crate::connection::Result;
use std::{future::Future, time::Duration};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_updater::{Update, UpdaterExt};

#[derive(Default)]
pub struct Updates {
    busy: tokio::sync::Mutex<()>,
    ready: tokio::sync::Mutex<Option<(Update, Vec<u8>)>>,
}

impl Updates {
    async fn check_once(&self, check: impl Future<Output = Result<()>>) -> Result<()> {
        let Ok(_busy) = self.busy.try_lock() else {
            return Ok(());
        };
        check.await
    }
}

pub async fn schedule(check: impl Fn()) {
    tokio::time::sleep(Duration::from_secs(8)).await;
    loop {
        check();
        tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
    }
}

pub fn check(app: &AppHandle, manual: bool) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<crate::Desktop>();
        if state
            .updates
            .check_once(check_update(&app, &state.updates, manual))
            .await
            .is_err()
        {
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

fn disabled_reason(appimage: bool, has_feed: bool) -> Option<&'static str> {
    if cfg!(target_os = "linux") && !appimage {
        Some("Updates require an installed AppImage.")
    } else if !has_feed {
        Some("This build has no update feed.")
    } else {
        None
    }
}

async fn check_update(app: &AppHandle, state: &Updates, manual: bool) -> Result<()> {
    let has_feed = app
        .config()
        .plugins
        .0
        .get("updater")
        .and_then(|config| config["endpoints"].as_array())
        .is_some_and(|endpoints| !endpoints.is_empty());
    if let Some(reason) = disabled_reason(std::env::var_os("APPIMAGE").is_some(), has_feed) {
        if manual {
            super::report(app, reason);
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
    install_if_accepted(receive, || {
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
    })
    .await
}

async fn install_if_accepted(
    response: tokio::sync::oneshot::Receiver<bool>,
    install: impl FnOnce() -> Result<()>,
) -> Result<()> {
    if response.await? {
        install()?;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[test]
    fn updater_rejects_stale_versions_and_requires_a_signature_for_the_announced_version(
    ) -> Result<()> {
        // Public test vectors from tauri-plugin-updater 2.14.1. Payload: b"test", version: 2.0.0.
        const PUBLIC_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEE0OUVGNDc0NkZBNkU2RDcKUldUWDVxWnZkUFNlcERHV2cyYno4bjZBclFuQk13Z1FLMWdlVm9zb3JjYkx6Y3VuRCtLSitjWWU=";
        const SIGNATURE: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVUWDVxWnZkUFNlcFBGMnpzanZQL0wvVFF0QkxSbldSdmFxKzRvbUsxVUV1Z3RRcC9VU3JHaUkxdjc2MnNEeTNpZDNhb3I3YlJDOUVUeWVvdDZsTUhIcldvWjVoalVNMVFjPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkxNjQ5MDUxCWZpbGU6cGF5bG9hZAl2ZXJzaW9uOjIuMC4wCkREcTJDTE9FbDVjUW1DMmVKUkhsblpQVlZpT1pzdTFDanBQS1FvZWk0QS9YQmZrRHFHTzBCK1FNcmNySGdFUTVxVjNtbmU3NXRkRldoSnBxVVhoOUFBPT0=";
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        context.package_info_mut().version = "1.9.0".parse()?;
        let config: tauri::Config = serde_json::from_str(include_str!("../tauri.conf.json"))?;
        context.config_mut().plugins = config.plugins;
        let updater_config = context
            .config_mut()
            .plugins
            .0
            .get_mut("updater")
            .ok_or("Missing updater config")?;
        assert_eq!(updater_config["requireSignedVersion"], true);
        updater_config["pubkey"] = serde_json::json!(PUBLIC_KEY);
        updater_config["endpoints"] = serde_json::json!(["https://github.com/labsiqbal/minarbot/releases/latest/download/tauri-latest-darwin-aarch64.json"]);
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(context)?;
        let updater = app.updater()?;
        let url = "https://github.com/labsiqbal/minarbot/releases/download/v2.0.0/Minarbot-Tauri-2.0.0-darwin-aarch64.dmg";
        let feed =
            |version| serde_json::json!({"version": version, "url": url, "signature": SIGNATURE});
        assert!(updater.restore_update(feed("1.8.0"))?.is_none());
        assert!(updater.restore_update(feed("1.9.0"))?.is_none());
        let update = updater
            .restore_update(feed("2.0.0"))?
            .ok_or("Newer update was not selected")?;
        assert_eq!(update.download_url.as_str(), url);
        assert!(allowed_download(&update.download_url));
        update.verify(b"test")?;
        assert!(update.verify(b"modified").is_err());
        let substituted = updater
            .restore_update(feed("2.1.0"))?
            .ok_or("Missing candidate update")?;
        assert!(matches!(
            substituted.verify(b"test"),
            Err(tauri_plugin_updater::Error::SignedVersionMismatch { .. })
        ));
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn checks_at_startup_and_every_six_hours() -> Result<()> {
        let checks = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&checks);
        let task = tokio::spawn(schedule(move || {
            count.fetch_add(1, Ordering::SeqCst);
        }));
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(7)).await;
        assert_eq!(checks.load(Ordering::SeqCst), 0);
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(checks.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(6 * 3600 - 1)).await;
        assert_eq!(checks.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(checks.load(Ordering::SeqCst), 2);
        task.abort();
        assert!(task.await.is_err_and(|error| error.is_cancelled()));
        Ok(())
    }

    #[tokio::test]
    async fn installation_requires_consent() -> Result<()> {
        for accepted in [false, true] {
            let (send, receive) = tokio::sync::oneshot::channel();
            send.send(accepted).map_err(|_| "Dialog receiver dropped")?;
            let mut installed = false;
            install_if_accepted(receive, || {
                installed = true;
                Ok(())
            })
            .await?;
            assert_eq!(installed, accepted);
        }
        Ok(())
    }

    #[tokio::test]
    async fn disabled_and_concurrent_checks_skip_and_failures_can_retry() -> Result<()> {
        assert_eq!(
            disabled_reason(true, false),
            Some("This build has no update feed.")
        );
        #[cfg(target_os = "linux")]
        assert_eq!(
            disabled_reason(false, true),
            Some("Updates require an installed AppImage.")
        );
        assert_eq!(disabled_reason(true, true), None);

        let state = Updates::default();
        let (started, start) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let first = state.check_once(async {
            started.send(()).map_err(|_| "Check observer dropped")?;
            released.await?;
            Err("Download failed".into())
        });
        let concurrent = async {
            start.await?;
            let result = state
                .check_once(async { Err("Concurrent check started".into()) })
                .await;
            release.send(()).map_err(|_| "First check dropped")?;
            result
        };
        let (first, concurrent) = tokio::join!(first, concurrent);
        concurrent?;
        assert_eq!(
            first.err().ok_or("First check succeeded")?.to_string(),
            "Download failed"
        );
        assert!(state
            .check_once(async { Err("Retry ran".into()) })
            .await
            .is_err());
        Ok(())
    }
}
