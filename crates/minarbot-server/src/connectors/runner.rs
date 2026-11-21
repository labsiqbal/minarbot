use super::journal::Journal;
use super::*;
use serde::Deserialize;
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Options {
    pub platform: String,
    pub directory: PathBuf,
    pub script_directory: PathBuf,
    pub agent: String,
    pub token: Option<String>,
    pub team: Option<String>,
    pub app: Option<String>,
    pub bot_token: Option<String>,
    pub app_token: Option<String>,
    pub users: Vec<Value>,
    #[serde(default)]
    pub channels: Vec<String>,
    pub origin: Option<String>,
}
#[derive(Clone)]
pub struct Local {
    pub origin: String,
    pub token: String,
    pub client: reqwest::Client,
}
impl Local {
    pub fn read(directory: &Path) -> crate::db::Result<Self> {
        let stat = std::fs::symlink_metadata(directory)?;
        if !stat.is_dir()
            || stat.uid() != rustix::process::geteuid().as_raw()
            || stat.mode() & 0o077 != 0
        {
            return Err("Script discovery is unavailable or unsafe.".into());
        }
        let mut raw = String::new();
        super::journal::private_file(&directory.join("connection.json"), 4096)?
            .read_to_string(&mut raw)?;
        let v: Value = serde_json::from_str(&raw)?;
        let origin = string(&v, "url");
        let token = string(&v, "token");
        let url = super::config::loopback(origin)?;
        if url.origin().ascii_serialization() != origin
            || !(32..=256).contains(&token.len())
            || !token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err("Script discovery is unavailable or unsafe.".into());
        }
        Ok(Self {
            origin: origin.into(),
            token: token.into(),
            client: http()?,
        })
    }
    pub async fn request(&self, path: &str, body: Option<&Value>) -> Result<Value> {
        let response = self
            .send(path, body.map(|v| v.to_string().into_bytes()), None)
            .await?;
        let bytes = bytes(response, 2 * 1024 * 1024).await?;
        serde_json::from_slice(&bytes).map_err(|_| error(502, "Invalid local agent response"))
    }
    pub async fn send(
        &self,
        path: &str,
        body: Option<Vec<u8>>,
        name: Option<&str>,
    ) -> Result<reqwest::Response> {
        let mut request = self
            .client
            .request(
                if body.is_some() {
                    reqwest::Method::POST
                } else {
                    reqwest::Method::GET
                },
                format!("{}{path}", self.origin),
            )
            .bearer_auth(&self.token)
            .header(
                "Content-Type",
                if name.is_some() {
                    "application/octet-stream"
                } else {
                    "application/json"
                },
            )
            .timeout(Duration::from_secs(15));
        if let Some(body) = body {
            request = request.body(body);
        }
        if let Some(name) = name {
            request = request.header("X-File-Name", encode(name));
        }
        let response = request
            .send()
            .await
            .map_err(|_| error(502, "Local agent request failed"))?;
        if !response.status().is_success() {
            return Err(error(
                response.status().as_u16(),
                format!(
                    "Local agent request rejected ({}).",
                    response.status().as_u16()
                ),
            ));
        }
        Ok(response)
    }
}
use std::os::unix::fs::MetadataExt;
pub fn http() -> crate::db::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}
pub fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}
pub async fn bytes(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    let mut result = vec![];
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| error(502, "Connector request outcome is uncertain."))?
    {
        if result.len() + chunk.len() > limit {
            return Err(error(413, "Connector response exceeds limit"));
        }
        result.extend_from_slice(&chunk);
    }
    Ok(result)
}
pub fn pattern(pattern: &str, value: &str) -> bool {
    regex::Regex::new(pattern).is_ok_and(|r| r.is_match(value))
}
pub fn number(v: &Value, k: &str) -> i64 {
    v[k].as_i64().unwrap_or(0)
}
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub fn initialize(db: &Connection, platform: &str) -> Result<()> {
    let legacy = exists(
        db,
        "SELECT 1 FROM sqlite_master WHERE name='slack_inbox'",
        [],
    )?;
    db.execute_batch(if platform == "slack" {
        include_str!("slack.sql")
    } else {
        include_str!("telegram.sql")
    })?;
    let columns = if platform == "slack" {
        vec![
            ("slack_inbox", "kind", "TEXT NOT NULL DEFAULT 'message'"),
            ("slack_inbox", "files", "TEXT NOT NULL DEFAULT '[]'"),
        ]
    } else {
        vec![
            (
                "telegram_questions",
                "choices",
                "TEXT NOT NULL DEFAULT '[]'",
            ),
            ("telegram_inbox", "checked", "INTEGER NOT NULL DEFAULT 0"),
        ]
    };
    for (table, column, kind) in columns {
        if !all(db, &format!("PRAGMA table_info({table})"), [])?
            .iter()
            .any(|r| r["name"] == column)
        {
            db.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {kind}"))?;
        }
    }
    if platform == "slack" {
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 1 {
            transaction(db, || {
                if legacy {
                    db.execute("UPDATE slack_inbox SET status='submitting' WHERE status='queued' AND substr(text,1,7)<>'!answer'",[])?;
                }
                db.execute_batch("PRAGMA user_version=1")?;
                Ok(())
            })?;
        }
    } else if all(db, "PRAGMA table_info(telegram_callbacks)", [])?
        .iter()
        .any(|r| r["name"] == "accepted")
    {
        db.execute_batch("ALTER TABLE telegram_callbacks RENAME COLUMN accepted TO selection")?;
    }
    Ok(())
}
pub async fn run() -> crate::db::Result<()> {
    let mut raw = String::new();
    std::io::stdin()
        .take(1024 * 1024)
        .read_to_string(&mut raw)?;
    let options: Options = serde_json::from_str(&raw)?;
    let shutdown = async {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {r=tokio::signal::ctrl_c()=>r, _=terminate.recv()=>Ok(())}
    };
    tokio::select! {result=run_options(&options)=>result,result=shutdown=>{result?;Ok(())}}
}
pub fn journal() -> crate::db::Result<()> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Command {
        directory: PathBuf,
        platform: String,
        command: String,
        id: Option<String>,
        revision: Option<i64>,
        action: Option<String>,
        confirmation: Option<String>,
    }
    let mut raw = String::new();
    std::io::stdin().take(8192).read_to_string(&mut raw)?;
    let c: Command = serde_json::from_str(&raw)?;
    if !["slack", "telegram"].contains(&c.platform.as_str())
        || !["status", "inspect", "recover"].contains(&c.command.as_str())
        || c.command != "status" && !c.id.as_deref().is_some_and(uuid)
    {
        return Err("Invalid arguments".into());
    }
    let owner = Journal::open(&c.directory, &c.platform)?;
    initialize(&owner.db, &c.platform).map_err(|e| e.message)?;
    let db = &owner.db;
    let p = &c.platform;
    let id = c.id.as_deref().unwrap_or("");
    let output = (|| -> Result<Value> {
        if c.command == "status" {
            let inbox = all(db, &format!("SELECT status,count(*) AS count FROM {p}_inbox GROUP BY status"), [])?;
            return if p == "slack" {
                Ok(json!({"sessionCooldowns":all(db,"SELECT method,until FROM slack_session_cooldowns ORDER BY method",[])?,"retryAt":one(db,"SELECT until FROM slack_delivery_cooldown WHERE id=1",[])?.map(|r|r["until"].clone()),"inbox":inbox,"outbox":all(db,"SELECT id,channel,root,status,part,timestamps,(SELECT count(*) FROM slack_recoveries WHERE reply=slack_outbox.id) AS revision FROM slack_outbox ORDER BY rowid DESC LIMIT 100",[])?}))
            } else {
                Ok(json!({"inbox":inbox,"outbox":all(db,"SELECT id,chat,reply_to,status,part,message_ids,error,(SELECT count(*) FROM telegram_recoveries WHERE reply=telegram_outbox.id) AS revision FROM telegram_outbox ORDER BY rowid DESC LIMIT 100",[])?}))
            };
        }
        if c.command == "inspect" {
            let reply = one(db, &format!("SELECT * FROM {p}_outbox WHERE id=?"), [id])?.ok_or_else(|| error(404,"Reply not found"))?;
            let decisions = all(db, &format!("SELECT * FROM {p}_recoveries WHERE reply=? ORDER BY revision"), [id])?;
            let mut output = json!({"reply":reply,"revision":decisions.len(),"decisions":decisions});
            if p == "slack" {
                output["document"] = one(db,"SELECT file,name,stage,remote FROM slack_documents WHERE id=?",[id])?.unwrap_or(Value::Null);
                output["retryAt"] = one(db,"SELECT until FROM slack_delivery_cooldown WHERE id=1",[])?.map(|r|r["until"].clone()).unwrap_or(Value::Null);
            } else if let Some(document) = one(db,"SELECT turn,thread,file,name,length(bytes) AS size FROM telegram_documents WHERE id=?",[id])? {
                output["document"] = document;
            }
            return Ok(output);
        }
        let action = c.action.as_deref().unwrap_or("");
        let revision = c.revision.filter(|n| (0..9_007_199_254_740_991).contains(n)).ok_or_else(|| error(400,"Invalid recovery decision"))?;
        if !matches!((action,c.confirmation.as_deref()),("retry",Some("--accept-duplicate-risk"))|("discard",Some("--confirm"))) {
            return Err(error(400,"Explicit recovery confirmation required"));
        }
        recover_reply(db, p, id, revision, action)
    })().map_err(|e| e.message)?;
    println!("{output}");
    Ok(())
}
pub(super) fn recover_reply(
    db: &Connection,
    p: &str,
    id: &str,
    revision: i64,
    action: &str,
) -> Result<Value> {
    transaction(db, || {
        let row =
            one(db, &format!("SELECT * FROM {p}_outbox WHERE id=?"), [id])?.ok_or_else(|| {
                error(
                    409,
                    "Delivery changed or is not recoverable; inspect it again",
                )
            })?;
        let current: i64 = db.query_row(
            &format!("SELECT count(*) FROM {p}_recoveries WHERE reply=?"),
            [id],
            |r| r.get(0),
        )?;
        if !["failed", "uncertain"].contains(&string(&row, "status")) || current != revision {
            return Err(error(
                409,
                "Delivery changed or is not recoverable; inspect it again",
            ));
        }
        if p == "slack" {
            db.execute(
                "INSERT INTO slack_recoveries(reply,revision,action,status,part) VALUES(?,?,?,?,?)",
                params![
                    id,
                    revision + 1,
                    action,
                    row["status"].as_str(),
                    row["part"].as_i64()
                ],
            )?;
        } else {
            db.execute("INSERT INTO telegram_recoveries(reply,revision,action,status,part,error) VALUES(?,?,?,?,?,?)",params![id,revision+1,action,row["status"].as_str(),row["part"].as_i64(),row["error"].as_str()])?;
        }
        let status = if action == "retry" {
            "queued"
        } else {
            "discarded"
        };
        db.execute(
            &format!(
                "UPDATE {p}_outbox SET status=?{} WHERE id=?",
                if p == "telegram" { ",error=NULL" } else { "" }
            ),
            [status, id],
        )?;
        Ok(json!({"id":id,"status":status,"revision":revision+1}))
    })
}
pub async fn run_options(options: &Options) -> crate::db::Result<()> {
    if !uuid(&options.agent)
        || options.users.is_empty()
        || options.users.len() > 100
        || options
            .users
            .iter()
            .enumerate()
            .any(|(i, u)| options.users[..i].contains(u))
    {
        return Err("Invalid connector binding or allowed users".into());
    }
    if !["slack", "telegram"].contains(&options.platform.as_str()) {
        return Err("Invalid connector platform".into());
    }
    let owner = Journal::open(&options.directory, &options.platform)?;
    initialize(&owner.db, &options.platform).map_err(|e| e.message)?;

    if options.platform == "telegram" {
        super::telegram::run(&owner.db, options)
            .await
            .map_err(|e| e.message.into())
    } else {
        super::slack::run(&owner.db, options)
            .await
            .map_err(|e| e.message.into())
    }
}
