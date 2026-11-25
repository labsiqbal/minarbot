//! Agent desktop fixture runtime (the Node `desktopFixture`).
//!
//! With `desktop.testRuntime`, the server drives each agent desktop through
//! the computer bridge instead of a sandboxed supervisor: `desktop-launch`
//! starts one (env and profile recorded), `desktop-capture` returns a
//! screenshot, every other `desktop-<operation>` is a native command
//! (recorded with its arguments), `desktop-stop` stops it and
//! `desktop-status` reports whether it exited. A test can make commands fail
//! or hold them until released.

use crate::{
    bridge::{Bridge, BridgeError, OperationFuture},
    Result,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

/// One `desktop-launch`.
#[derive(Clone, Debug)]
pub struct Launch {
    pub id: String,
    pub env: Value,
    pub profile: Value,
}

/// One native command (`desktop-<operation>` other than capture/stop/status).
#[derive(Clone, Debug)]
pub struct Command {
    pub operation: String,
    pub args: Vec<String>,
}

impl Command {
    /// The operation and its arguments as one JSON text, for searching.
    pub fn text(&self) -> String {
        json!({"operation": self.operation, "args": self.args}).to_string()
    }
}

#[derive(Default)]
struct State {
    launches: Vec<Launch>,
    commands: Vec<Command>,
    stopped: HashSet<String>,
    fail: bool,
    /// While set, commands wait until it turns false.
    gate: Option<watch::Sender<bool>>,
    admitted: usize,
}

/// Running desktop fixture; give [`DesktopFixture::bridge`] to
/// `ServerConfig::computer_bridge` and [`DesktopFixture::options`] as the
/// `desktop` option.
#[derive(Clone)]
pub struct DesktopFixture {
    bridge: Bridge,
    state: Arc<Mutex<State>>,
    directory: Arc<Directory>,
}

/// Short temporary directory, removed with the last fixture clone. Desktop
/// sockets live below it (`leases/v-<id>/proxy.sock`,
/// `agent-desktops/<agent>/x11/vnc.sock`) and must stay within the 108-byte
/// Unix socket path limit, which a test root's path would exceed.
struct Directory(PathBuf);

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl DesktopFixture {
    /// Fixture whose screenshots are `png`.
    pub async fn start(png: Vec<u8>) -> Result<Self> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let directory = std::env::temp_dir().join(format!("mbd-{}", &id[..12]));
        std::fs::create_dir(&directory)?;
        let directory = Arc::new(Directory(directory.canonicalize()?));
        let state: Arc<Mutex<State>> = Arc::default();
        let shared = state.clone();
        let screenshot = STANDARD.encode(png);
        let bridge = Bridge::operations_async(move |operation, args| {
            let shared = shared.clone();
            let screenshot = screenshot.clone();
            let future: OperationFuture = Box::pin(async move {
                let id = args["id"].as_str().unwrap_or_default().to_owned();
                let with = |change: &mut dyn FnMut(&mut State)| {
                    if let Ok(mut state) = shared.lock() {
                        change(&mut state);
                    }
                };
                match operation.as_str() {
                    "desktop-launch" => {
                        with(&mut |state| {
                            state.launches.push(Launch {
                                id: id.clone(),
                                env: args["env"].clone(),
                                profile: args["profile"].clone(),
                            });
                        });
                        Ok(Value::Null)
                    }
                    "desktop-capture" => Ok(json!(screenshot)),
                    "desktop-stop" => {
                        with(&mut |state| {
                            state.stopped.insert(id.clone());
                        });
                        Ok(Value::Null)
                    }
                    "desktop-status" => {
                        let mut stopped = false;
                        with(&mut |state| stopped = state.stopped.contains(&id));
                        Ok(json!(stopped))
                    }
                    other => {
                        let operation = other.strip_prefix("desktop-").unwrap_or(other).to_owned();
                        let mut gate = None;
                        with(&mut |state| {
                            state.admitted += 1;
                            gate = state.gate.as_ref().map(watch::Sender::subscribe);
                        });
                        if let Some(mut gate) = gate {
                            let _ = gate.wait_for(|held| !*held).await;
                        }
                        let mut fail = false;
                        with(&mut |state| {
                            state.commands.push(Command {
                                operation: operation.clone(),
                                args: args["args"]
                                    .as_array()
                                    .map(|args| {
                                        args.iter()
                                            .map(|arg| {
                                                arg.as_str()
                                                    .map_or_else(|| arg.to_string(), str::to_owned)
                                            })
                                            .collect()
                                    })
                                    .unwrap_or_default(),
                            });
                            fail = state.fail;
                        });
                        if fail {
                            return Err(BridgeError::new(500, "synthetic process failure"));
                        }
                        Ok(json!(""))
                    }
                }
            });
            future
        })
        .await?;
        Ok(Self {
            bridge,
            state,
            directory,
        })
    }

    pub fn bridge(&self) -> &Bridge {
        &self.bridge
    }

    /// Desktop data directory (`desktop.dataDir`).
    pub fn directory(&self) -> &Path {
        &self.directory.0
    }

    /// The `desktop` server option for this fixture plus `extra` fields.
    pub fn options(&self, extra: Value) -> Value {
        let mut options = json!({"dataDir": self.directory(),
            "leaseDir": self.directory().join("leases"), "enabled": true, "testRuntime": true,
            "idleMs": 1000});
        if let (Some(options), Some(extra)) = (options.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                options.insert(key.clone(), value.clone());
            }
        }
        options
    }

    fn with<T>(&self, read: impl FnOnce(&mut State) -> T) -> T {
        match self.state.lock() {
            Ok(mut state) => read(&mut state),
            Err(poisoned) => read(&mut poisoned.into_inner()),
        }
    }

    pub fn launches(&self) -> Vec<Launch> {
        self.with(|state| state.launches.clone())
    }

    pub fn commands(&self) -> Vec<Command> {
        self.with(|state| state.commands.clone())
    }

    /// Every later command fails with "synthetic process failure".
    pub fn fail(&self) {
        self.with(|state| state.fail = true);
    }

    /// Hold every command until [`DesktopFixture::release`].
    pub fn hold(&self) {
        self.with(|state| state.gate = Some(watch::channel(true).0));
    }

    /// Commands that reached the fixture so far, held or not.
    pub fn admitted(&self) -> usize {
        self.with(|state| state.admitted)
    }

    pub fn release(&self) {
        if let Some(gate) = self.with(|state| state.gate.take()) {
            gate.send_replace(false);
        }
    }
}
