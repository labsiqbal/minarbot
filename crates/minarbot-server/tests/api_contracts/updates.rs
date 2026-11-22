use super::{id, json};
use crate::server::{Result, TestRoot};
use minarbot_launcher::files;
use reqwest::header;
use rustix::process::Signal;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    task::JoinHandle,
};

struct Controller {
    commands: Arc<Mutex<BTreeMap<String, Value>>>,
    task: JoinHandle<Result<()>>,
}

impl Controller {
    fn start(path: &Path) -> Result<Self> {
        let listener = UnixListener::bind(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        let commands = Arc::new(Mutex::new(BTreeMap::new()));
        let recorded = commands.clone();
        let task = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await?;
                receive(socket, &recorded).await?;
            }
        });
        Ok(Self { commands, task })
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn receive(socket: UnixStream, recorded: &Mutex<BTreeMap<String, Value>>) -> Result<()> {
    let mut socket = BufReader::new(socket);
    let mut data = String::new();
    socket.read_line(&mut data).await?;
    let request: Value = serde_json::from_str(&data)?;
    let reply = match request["type"].as_str() {
        Some("status") => json!({"protocol":1,"current":"development"}),
        Some("apply") => {
            let id = request["id"].as_str().ok_or("missing apply ID")?;
            let mut commands = recorded.lock().map_err(|_| "control log poisoned")?;
            if commands
                .get(id)
                .is_some_and(|previous| previous != &request)
            {
                json!({"error":"Request ID was already used."})
            } else {
                commands.insert(id.to_owned(), request.clone());
                json!({"accepted":true,"id":id})
            }
        }
        _ => return Err("unexpected launcher request".into()),
    };
    socket.write_all(format!("{reply}\n").as_bytes()).await?;
    Ok(())
}

#[tokio::test]
async fn owner_update_api_validates_request_and_replays_launcher_id() -> Result<()> {
    let root = TestRoot::new()?;
    let releases = root.path().join("releases");
    let release = releases.join("1.0.1-12345678");
    fs::create_dir_all(release.join("bin"))?;
    fs::create_dir(releases.join("feed"))?;
    let binary = release.join("bin/minarbot-server");
    fs::write(&binary, b"fixture binary")?;
    fs::set_permissions(binary, fs::Permissions::from_mode(0o700))?;
    fs::write(release.join("package.json"), br#"{"version":"1.0.1"}"#)?;
    let manifest = files::manifest(&release).map_err(|error| error.to_string())?;
    let metadata = json!({
        "id":"1.0.1-12345678","version":"1.0.1","commit":"1234567812345678123456781234567812345678",
        "created":"2026-01-01T00:00:00Z","node":"fixture","notes":"Fixture 1.0.1","files":manifest
    });
    let metadata = serde_json::to_vec(&metadata)?;
    fs::write(release.join("release.json"), &metadata)?;
    fs::write(releases.join("feed/latest.json"), &metadata)?;
    let socket = root.path().join("control.sock");
    let ctl = Controller::start(&socket)?;
    let app = root
        .start(json!({"updates":{"releasesDir":releases,"launcherSocket":socket}}))
        .await?;
    let path = "/api/updates";
    let apply = "/api/updates/apply";
    let state = json(app.get(path), 200).await?;
    assert_eq!(state["launcherAvailable"], true);
    assert_eq!(state["available"]["version"], "1.0.1");
    assert_eq!(state["feed"]["version"], "1.0.1");
    let command = json!({"id":id(),"version":"1.0.1"});
    for _ in 0..2 {
        json(app.post(apply).json(&command), 202).await?;
    }
    {
        let commands = ctl.commands.lock().map_err(|_| "control log poisoned")?;
        assert_eq!(commands.len(), 1);
        let key = command["id"].as_str().ok_or("missing apply ID")?;
        assert_eq!(commands[key]["target"], "1.0.1-12345678");
        assert_eq!(commands[key]["version"], "1.0.1");
    }
    let mut extra = command.clone();
    extra["extra"] = json!(1);
    json(app.post(apply).json(&extra), 400).await?;
    json(
        app.post(apply).json(&json!({"id":"bad","version":"1.0.1"})),
        400,
    )
    .await?;
    json(
        app.client
            .post(format!("{}{apply}", app.origin))
            .header(header::ORIGIN, "https://foreign.invalid")
            .json(&command),
        403,
    )
    .await?;
    json(
        app.post(apply)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(command.to_string()),
        403,
    )
    .await?;
    for (header, value) in [
        (header::AUTHORIZATION, "Bearer peer-canary"),
        (header::ORIGIN, "https://foreign.invalid"),
        (header::HOST, "foreign.invalid"),
    ] {
        json(app.get(path).header(header, value), 403).await?;
    }
    assert!(app.terminate(Signal::TERM).await?.success());
    let app = root
        .start(json!({"updates":{"releasesDir":releases}}))
        .await?;
    assert_eq!(json(app.get(path), 200).await?["launcherAvailable"], false);
    let denied = json(app.post(apply).json(&command), 409).await?;
    assert!(denied["error"]
        .as_str()
        .ok_or("missing update error")?
        .contains("launcher"));
    Ok(())
}
