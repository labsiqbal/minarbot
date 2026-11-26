//! Shared pieces of the Slack and Telegram media ports (`rg_slack_media`,
//! `rg_telegram_media`): a recorder between the connector's script proxy
//! and Minarbot (the Node fixture's `requests`), the PDF fault switch, and
//! journal polling.

use minarbot_testkit::{
    bridge::Bridge,
    db::{Inspector, Params},
    platform::{forward, ConnectorFixture, Fault},
    prelude::*,
    stub::{Stub, StubReply},
};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

/// One script request the connector sent (`connector.requests`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptRequest {
    pub method: String,
    pub path: String,
    pub bytes: Vec<u8>,
    pub name: Option<String>,
}

/// Records every script request, then forwards it to Minarbot.
pub struct Recorder {
    stub: Stub,
    requests: Arc<Mutex<Vec<ScriptRequest>>>,
}

impl Recorder {
    pub async fn start(app_origin: &str) -> Result<Self> {
        let requests: Arc<Mutex<Vec<ScriptRequest>>> = Arc::default();
        let log = requests.clone();
        let app = app_origin.to_owned();
        let client = minarbot_testkit::http::Client::builder()
            .no_proxy()
            .build()?;
        let stub = Stub::start_async(move |request| {
            let (log, app, client) = (log.clone(), app.clone(), client.clone());
            async move {
                if let Ok(mut log) = log.lock() {
                    log.push(ScriptRequest {
                        method: request.method.clone(),
                        path: request.target.clone(),
                        bytes: request.body.clone(),
                        name: request.header("x-file-name").map(str::to_owned),
                    });
                }
                forward(&client, &app, &request)
                    .await
                    .unwrap_or(StubReply::Drop)
            }
        })
        .await?;
        Ok(Self { stub, requests })
    }

    pub fn origin(&self) -> &str {
        self.stub.origin()
    }

    pub fn requests(&self) -> Vec<ScriptRequest> {
        self.requests
            .lock()
            .map(|requests| requests.clone())
            .unwrap_or_default()
    }

    /// `uploads()`: file uploads (`POST .../files/<id>`).
    pub fn uploads(&self) -> Vec<ScriptRequest> {
        self.requests()
            .into_iter()
            .filter(|request| request.method == "POST" && upload_path(&request.path))
            .collect()
    }
}

/// `/\/files\/[^/]+$/`
pub fn upload_path(path: &str) -> bool {
    path.rsplit_once("/files/")
        .is_some_and(|(_, file)| !file.is_empty() && !file.contains('/'))
}

fn turns_path(path: &str) -> bool {
    path.ends_with("/turns")
}

/// `armFaults()`: lose the reply of the next upload and the next turn.
pub fn arm_faults(connector: &ConnectorFixture) {
    for matches in [upload_path as fn(&str) -> bool, turns_path] {
        connector.push_fault(Fault {
            method: Some("POST"),
            matches,
            after: true,
        });
    }
}

/// `expectedFailure(pattern)`: the connector stops with `message` in stderr.
pub async fn expected_failure(connector: &mut ConnectorFixture, message: &str) -> Result<()> {
    let stderr = connector.expected_failure().await?;
    ensure!(
        stderr.contains(message),
        "connector error {stderr:?} lacks {message:?}"
    );
    Ok(())
}

/// The PDF fault switch (`pdfFault` returning 503 while unavailable).
pub async fn pdf_switch() -> Result<(Bridge, Arc<AtomicBool>)> {
    let unavailable = Arc::new(AtomicBool::new(false));
    let flag = unavailable.clone();
    let bridge = Bridge::pdf(move |_| {
        flag.load(Ordering::SeqCst)
            .then(|| (503, "PDF sandbox is unavailable.".to_owned()))
    })
    .await?;
    Ok((bridge, unavailable))
}

pub fn journal(connector: &ConnectorFixture) -> PathBuf {
    connector
        .directory
        .join(format!("{}.sqlite", connector.kind.name()))
}

/// One journal row, if present.
pub fn journal_row(
    connector: &ConnectorFixture,
    sql: &str,
    params: impl Params,
) -> Result<Option<Value>> {
    Ok(Inspector::open(journal(connector))?
        .rows(sql, params)?
        .into_iter()
        .next())
}

/// `settle(turn)`: the inbox row is done and its outbox reply was sent.
pub async fn settle(connector: &mut ConnectorFixture, turn: &Value) -> Result<()> {
    let turn = turn.as_str().ok_or("turn")?.to_owned();
    let prefix = connector.kind.name();
    let deadline = tokio::time::Instant::now() + minarbot_testkit::WAIT;
    loop {
        connector.check()?;
        let db = Inspector::open(journal(connector))?;
        let inbox = db.rows(
            &format!("SELECT status FROM {prefix}_inbox WHERE turn=?"),
            [&turn],
        )?;
        let outbox = db.rows(
            &format!("SELECT status FROM {prefix}_outbox WHERE id=?"),
            [&turn],
        )?;
        if inbox.first().is_some_and(|row| row["status"] == "done")
            && outbox.first().is_some_and(|row| row["status"] == "sent")
        {
            return Ok(());
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "Media task did not settle: inbox {inbox:?}, outbox {outbox:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// Serialized model requests (`JSON.stringify(captures[i])`).
pub fn capture(requests: &[minarbot_testkit::model::ModelRequest], index: usize) -> String {
    requests
        .get(index)
        .map(|request| request.body.to_string())
        .unwrap_or_default()
}
