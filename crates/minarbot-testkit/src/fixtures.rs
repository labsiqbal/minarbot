use crate::server::Result;
use axum::{
    body::Body,
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::net::TcpListener;
use uuid::Uuid;

pub struct SqliteInspector {
    connection: Connection,
}

impl SqliteInspector {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            connection: Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?,
        })
    }

    pub fn scalar<T: rusqlite::types::FromSql>(&self, sql: &str) -> Result<T> {
        Ok(self.connection.query_row(sql, [], |row| row.get(0))?)
    }

    pub fn table_count(&self, table: &str) -> Result<i64> {
        let quoted = table.replace('"', "\"\"");
        Ok(self
            .connection
            .query_row(&format!("SELECT count(*) FROM \"{quoted}\""), [], |row| {
                row.get(0)
            })?)
    }

    pub fn rows(&self, sql: &str) -> Result<Vec<Value>> {
        let mut statement = self.connection.prepare(sql)?;
        let names = statement
            .column_names()
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>();
        let rows = statement
            .query_map([], |row| {
                let mut object = serde_json::Map::new();
                for (index, name) in names.iter().enumerate() {
                    let value: rusqlite::types::Value = row.get(index)?;
                    let value = match value {
                        rusqlite::types::Value::Null => Value::Null,
                        rusqlite::types::Value::Integer(value) => json!(value),
                        rusqlite::types::Value::Real(value) => json!(value),
                        rusqlite::types::Value::Text(value) => json!(value),
                        rusqlite::types::Value::Blob(value) => json!(base64(&value)),
                    };
                    object.insert(name.clone(), value);
                }
                Ok(Value::Object(object))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let first = chunk.first().copied().unwrap_or(0);
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        out.push(ALPHABET[(first >> 2) as usize] as char);
        out.push(ALPHABET[((first & 3) << 4 | second >> 4) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((second & 15) << 2 | third >> 6) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(third & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

pub struct TempWorkspace {
    path: PathBuf,
}

/// Locate one of the kit's native fake peer binaries from a test process.
pub fn fake_binary(name: &str) -> Result<PathBuf> {
    let executable = std::env::current_exe()?;
    let target = executable
        .parent()
        .and_then(Path::parent)
        .ok_or("test executable has no target directory")?;
    Ok(target.join(name))
}

impl TempWorkspace {
    pub fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!("minarbot-workspace-{}", Uuid::new_v4()));
        fs::create_dir(&path)?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write(&self, relative: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> Result<PathBuf> {
        let path = self.path.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, contents)?;
        Ok(path)
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[derive(Clone)]
struct ModelState {
    replies: Arc<Mutex<VecDeque<Value>>>,
    requests: Arc<Mutex<Vec<Value>>>,
}

pub struct ScriptedModel {
    pub origin: String,
    state: ModelState,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl ScriptedModel {
    pub async fn start() -> Result<Self> {
        let state = ModelState {
            replies: Arc::new(Mutex::new(VecDeque::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
        };
        let app = Router::new()
            .route("/v1/chat/completions", post(model_reply))
            .with_state(state.clone());
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let task = tokio::spawn(async move { axum::serve(listener, app).await });
        Ok(Self {
            origin,
            state,
            task,
        })
    }

    pub fn push(&self, reply: Value) -> Result<()> {
        self.state
            .replies
            .lock()
            .map_err(|_| "model reply queue poisoned")?
            .push_back(reply);
        Ok(())
    }

    pub fn requests(&self) -> Result<Vec<Value>> {
        Ok(self
            .state
            .requests
            .lock()
            .map_err(|_| "model request log poisoned")?
            .clone())
    }
}

async fn model_reply(State(state): State<ModelState>, Json(body): Json<Value>) -> Response {
    if let Ok(mut requests) = state.requests.lock() {
        requests.push(body.clone());
    }
    let reply = state
        .replies
        .lock()
        .ok()
        .and_then(|mut queue| queue.pop_front())
        .unwrap_or_else(|| json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"fixture"}}]}));
    if body.get("stream").and_then(Value::as_bool).unwrap_or(false) {
        let content = reply["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("");
        let chunks = content.chars().map(|character| format!("data: {}\n\n", json!({"choices":[{"delta":{"content":character.to_string()},"finish_reason":Value::Null}]}))).collect::<String>();
        let done = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"delta":{},"finish_reason":"stop"}]})
        );
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(format!("{chunks}{done}")))
            .unwrap_or_else(|_| Response::new(Body::from("data: [DONE]\n\n")));
    }
    Json(reply).into_response()
}

impl Drop for ScriptedModel {
    fn drop(&mut self) {
        self.task.abort();
    }
}
