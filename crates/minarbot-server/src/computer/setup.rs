use super::{
    error,
    runtime::{program, Runtime, REPOSITORY},
    Result,
};
use crate::{web::Failure, AppState};
use serde_json::{json, Value};
use std::{process::Stdio, sync::Mutex};
use tokio::process::Command;

#[derive(Clone)]
pub(super) enum Backend {
    Host,
    Container { runtime: Runtime, image: String },
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    NoRuntime,
    ImageMissing,
    Downloading,
    Ready,
    Failed,
}

struct Status {
    phase: Phase,
    runtime: Option<&'static str>,
    missing: Vec<&'static str>,
    can_setup: bool,
    message: Option<String>,
    backend: Option<Backend>,
}
impl Status {
    fn value(&self) -> Value {
        let phase = match self.phase {
            Phase::NoRuntime => "no_runtime",
            Phase::ImageMissing => "image_missing",
            Phase::Downloading => "downloading",
            Phase::Ready => "ready",
            Phase::Failed => "failed",
        };
        json!({"state":phase,"runtime":self.runtime,"missingPrograms":self.missing,
            "canSetup":self.can_setup,"message":self.message})
    }
}

pub(super) struct Setup {
    status: Mutex<Status>,
    override_enabled: Option<bool>,
    fixture: bool,
}
impl Setup {
    pub fn new(override_enabled: Option<bool>, fixture: bool) -> Self {
        let supported = crate::platform::LINUX_SANDBOX;
        let disabled = !supported || override_enabled == Some(false);
        let ready = supported && fixture && override_enabled == Some(true);
        let status = Status {
            phase: if disabled {
                Phase::Failed
            } else if ready {
                Phase::Ready
            } else {
                Phase::NoRuntime
            },
            runtime: None,
            missing: Vec::new(),
            can_setup: !disabled && !ready,
            message: if !supported {
                Some(crate::platform::COMPUTER_UNAVAILABLE.into())
            } else {
                disabled.then(|| "Computer is disabled by MINARBOT_AGENT_DESKTOP.".into())
            },
            backend: ready.then_some(Backend::Host),
        };
        Self {
            status: Mutex::new(status),
            override_enabled,
            fixture,
        }
    }
    pub fn backend(&self) -> Option<Backend> {
        self.status.lock().ok()?.backend.clone()
    }
    pub fn status(&self) -> Result<Value> {
        Ok(self.status.lock().map_err(|_| Failure::storage())?.value())
    }
    #[cfg(test)]
    pub fn set_backend(&self, backend: Backend) {
        if let Ok(mut status) = self.status.lock() {
            status.backend = Some(backend);
            status.phase = Phase::Ready;
        }
    }
}

struct Requirement {
    programs: &'static [&'static str],
    packages: [&'static str; 3],
}

const REQUIREMENTS: &[Requirement] = &[
    Requirement {
        programs: &["bwrap"],
        packages: ["bubblewrap"; 3],
    },
    Requirement {
        programs: &["Xvnc", "Xtigervnc"],
        packages: ["tigervnc-standalone-server", "tigervnc", "tigervnc-server"],
    },
    Requirement {
        programs: &["dbus-daemon"],
        packages: ["dbus-daemon", "dbus", "dbus-daemon"],
    },
    Requirement {
        programs: &["xfwm4"],
        packages: ["xfwm4"; 3],
    },
    Requirement {
        programs: &["xfdesktop"],
        packages: ["xfdesktop4", "xfdesktop", "xfdesktop"],
    },
    Requirement {
        programs: &["xfce4-panel"],
        packages: ["xfce4-panel"; 3],
    },
    Requirement {
        programs: &["xfce4-terminal"],
        packages: ["xfce4-terminal"; 3],
    },
    Requirement {
        programs: &["xdotool"],
        packages: ["xdotool"; 3],
    },
    Requirement {
        programs: &["scrot"],
        packages: ["scrot"; 3],
    },
    Requirement {
        programs: &["google-chrome", "google-chrome-stable", "chromium"],
        packages: ["chromium"; 3],
    },
];

fn missing() -> Vec<&'static Requirement> {
    REQUIREMENTS
        .iter()
        .filter(|item| program(item.programs).is_none())
        .collect()
}

fn installer(missing: &[&Requirement]) -> Option<Command> {
    let pkexec = program(&["pkexec"])?;
    let (index, executable, arguments) = [
        ("apt-get", &["install", "-y"][..]),
        ("pacman", &["-S", "--needed", "--noconfirm"][..]),
        ("dnf", &["install", "-y"][..]),
    ]
    .into_iter()
    .enumerate()
    .find_map(|(index, (name, args))| Some((index, program(&[name])?, args)))?;
    let mut command = Command::new(pkexec);
    command
        .arg("--disable-internal-agent")
        .arg(executable)
        .args(arguments);
    command
        .arg("--")
        .args(missing.iter().map(|item| item.packages[index]));
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Some(command)
}

pub(super) fn begin(
    state: &AppState,
    explicit: bool,
) -> Result<Option<tokio::task::JoinHandle<()>>> {
    if !crate::platform::LINUX_SANDBOX {
        return if explicit {
            Err(error(503, crate::platform::COMPUTER_UNAVAILABLE))
        } else {
            Ok(None)
        };
    }
    let setup = &state.computer.setup;
    let mut status = setup.status.lock().map_err(|_| Failure::storage())?;
    if setup.override_enabled == Some(false)
        || status.phase == Phase::Downloading
        || status.backend.is_some()
        || setup.fixture
    {
        return Ok(None);
    }
    status.phase = Phase::Downloading;
    status.message = None;
    drop(status);
    let state = state.clone();
    Ok(Some(tokio::spawn(async move {
        let result = prepare(&state, explicit).await;
        if let Ok(mut status) = state.computer.setup.status.lock() {
            match result {
                Ok(phase) => status.phase = phase,
                Err(failure) => {
                    status.phase = Phase::Failed;
                    status.message = Some(failure.message);
                }
            }
        }
    })))
}

async fn prepare(state: &AppState, explicit: bool) -> Result<Phase> {
    let setup = &state.computer.setup;
    let enabled = setup.override_enabled.unwrap_or({
        let db = state.database.lock().map_err(|_| Failure::storage())?;
        db.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM computer_settings WHERE id=1 AND enabled=1)",
            [],
            |row| row.get(0),
        )?
    });
    let runtime = Runtime::detect().await;
    let mut requirements = if runtime.is_none() {
        missing()
    } else {
        Vec::new()
    };
    let install = installer(&requirements);
    {
        let mut status = setup.status.lock().map_err(|_| Failure::storage())?;
        status.runtime = runtime.as_ref().map(Runtime::label);
        status.missing = requirements.iter().map(|item| item.programs[0]).collect();
    }
    let backend = if let Some(runtime) = runtime {
        if !explicit && !enabled {
            return Ok(Phase::ImageMissing);
        }
        let cache =
            minarbot_launcher::files::sidecar(&state.config.database, ".computer-image.json");
        let digest = crate::updates::computer_image(state, &cache).await?;
        let image = format!("{REPOSITORY}@{digest}");
        if !runtime.has_image(&image).await {
            runtime.pull(&image).await?;
        }
        Backend::Container { runtime, image }
    } else {
        if explicit && !requirements.is_empty() {
            let mut install = install.ok_or_else(|| {
                error(
                    503,
                    "Install the missing desktop programs or Podman, then try again.",
                )
            })?;
            let result = install.status().await;
            requirements = missing();
            let mut status = setup.status.lock().map_err(|_| Failure::storage())?;
            status.missing = requirements.iter().map(|item| item.programs[0]).collect();
            if !result.is_ok_and(|result| result.success()) || !requirements.is_empty() {
                return Err(error(503, "Desktop programs could not be installed. Check the system password prompt and try again."));
            }
        }
        if !requirements.is_empty() || !explicit && !enabled {
            return Ok(Phase::NoRuntime);
        }
        Backend::Host
    };
    if explicit {
        let db = state.database.lock().map_err(|_| Failure::storage())?;
        db.connection
            .execute("INSERT OR REPLACE INTO computer_settings VALUES(1,1)", [])?;
    }
    let mut status = setup.status.lock().map_err(|_| Failure::storage())?;
    status.backend = Some(backend);
    status.phase = Phase::Ready;
    status.can_setup = false;
    Ok(Phase::Ready)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer::desktop;
    use axum::{body::Body, http::Request};

    #[tokio::test]
    async fn readiness_endpoint_and_owner_origin() -> crate::db::Result<()> {
        let scratch = desktop::tests::Scratch::new()?;
        let config = serde_json::from_value::<crate::config::Config>(
            json!({"database":scratch.0.join("readiness.sqlite")}),
        )?;
        let setup = desktop::Manager::new(&config)?;
        let state = AppState {
            computer: std::sync::Arc::new(setup),
            database: std::sync::Arc::new(Mutex::new(crate::db::open(&config.database, false, 0)?)),
            config: std::sync::Arc::new(config),
            origin: "http://127.0.0.1:43177".into(),
            auth: None,
            events: crate::events::Hub::new(),
            turns: crate::turns::Runtime::default(),
            trial_pending: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            whatsapp: Default::default(),
        };
        for (phase, name) in [
            (Phase::NoRuntime, "no_runtime"),
            (Phase::ImageMissing, "image_missing"),
            (Phase::Downloading, "downloading"),
            (Phase::Ready, "ready"),
            (Phase::Failed, "failed"),
        ] {
            state
                .computer
                .setup
                .status
                .lock()
                .map_err(|_| "setup lock")?
                .phase = phase;
            let request = Request::builder()
                .uri("/api/settings/computer")
                .header("Host", "127.0.0.1:43177")
                .body(Body::empty())?;
            let response = super::super::entry(axum::extract::State(state.clone()), request).await;
            assert_eq!(response.status(), 200);
            let bytes = axum::body::to_bytes(response.into_body(), 2048).await?;
            assert_eq!(serde_json::from_slice::<Value>(&bytes)?["state"], name);
        }
        for (origin, body, status) in [
            ("https://foreign.invalid", "{}", 403),
            (state.origin.as_str(), r#"{"image":"untrusted"}"#, 400),
            (state.origin.as_str(), "{}", 202),
        ] {
            let request = Request::builder()
                .method("POST")
                .uri("/api/settings/computer")
                .header("Host", "127.0.0.1:43177")
                .header("Origin", origin)
                .header("Content-Type", "application/json")
                .body(Body::from(body))?;
            assert_eq!(
                super::super::entry(axum::extract::State(state.clone()), request)
                    .await
                    .status(),
                status
            );
        }
        Ok(())
    }
}
