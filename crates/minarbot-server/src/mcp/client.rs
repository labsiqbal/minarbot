use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
static LIMIT: std::sync::OnceLock<Arc<Semaphore>> = std::sync::OnceLock::new();
fn failed() -> Failure {
    error(502, "MCP request failed.")
}
pub struct Session {
    client: reqwest::Client,
    url: String,
    token: String,
    id: Option<String>,
    seq: i64,
    bytes: usize,
    pub capabilities: Value,
    _permit: OwnedSemaphorePermit,
}
impl Session {
    pub async fn open(url: &str, token: &str) -> Result<Self> {
        let permit = LIMIT
            .get_or_init(|| Arc::new(Semaphore::new(4)))
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                error(
                    429,
                    "Four MCP connections are in use. Try again after they finish.",
                )
            })?;
        let mut s = Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(20))
                .build()
                .map_err(|_| failed())?,
            url: url.into(),
            token: token.into(),
            id: None,
            seq: 0,
            bytes: 0,
            capabilities: Value::Null,
            _permit: permit,
        };
        let init=s.rpc("initialize",json!({"protocolVersion":"2025-03-26","capabilities":{"extensions":{"io.modelcontextprotocol/ui":{}}},"clientInfo":{"name":"minarbot","version":"0.1.0"}})).await?;
        s.capabilities = init["capabilities"].clone();
        s.notify("notifications/initialized").await?;
        Ok(s)
    }
    fn request(&self, method: reqwest::Method) -> reqwest::RequestBuilder {
        let mut r = self
            .client
            .request(method, &self.url)
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-03-26");
        if !self.token.is_empty() {
            r = r.bearer_auth(&self.token);
        }
        if let Some(id) = &self.id {
            r = r.header("Mcp-Session-Id", id);
        }
        r
    }
    async fn notify(&self, method: &str) -> Result<()> {
        let r = self
            .request(reqwest::Method::POST)
            .json(&json!({"jsonrpc":"2.0","method":method}))
            .send()
            .await
            .map_err(|_| failed())?;
        if !r.status().is_success() {
            return Err(failed());
        }
        Ok(())
    }
    pub async fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        self.rpc_with_limit(method, params, None).await
    }
    pub async fn read_app(&mut self, uri: &str, limit: usize) -> Result<Value> {
        self.rpc_with_limit("resources/read", json!({"uri":uri}), Some(limit))
            .await
    }
    async fn rpc_with_limit(
        &mut self,
        method: &str,
        params: Value,
        document_limit: Option<usize>,
    ) -> Result<Value> {
        self.seq += 1;
        let id = self.seq;
        let mut response = self
            .request(reqwest::Method::POST)
            .json(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .send()
            .await
            .map_err(|_| failed())?;
        if !response.status().is_success() {
            return Err(failed());
        }
        if method == "initialize" {
            self.id = response
                .headers()
                .get("Mcp-Session-Id")
                .and_then(|v| v.to_str().ok())
                .filter(|s| s.len() <= 256 && !s.chars().any(char::is_control))
                .map(str::to_owned);
        }
        let sse = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"));
        let mut bytes = vec![];
        while let Some(chunk) = response.chunk().await.map_err(|_| failed())? {
            if document_limit.is_none() {
                self.bytes += chunk.len();
            }
            if document_limit.map_or(self.bytes > 256 * 1024, |limit| {
                bytes.len() + chunk.len() > limit
            }) {
                return Err(failed());
            }
            bytes.extend_from_slice(&chunk);
        }
        let body: Value = if sse {
            let raw = std::str::from_utf8(&bytes).map_err(|_| failed())?;
            raw.replace("\r\n", "\n")
                .split("\n\n")
                .filter_map(|e| {
                    let data = e
                        .lines()
                        .filter_map(|l| l.strip_prefix("data:").map(str::trim_start))
                        .collect::<Vec<_>>()
                        .join("\n");
                    serde_json::from_str::<Value>(&data).ok()
                })
                .find(|v| v["id"] == id)
                .ok_or_else(failed)?
        } else {
            serde_json::from_slice(&bytes).map_err(|_| failed())?
        };
        if body["id"] != id || body["jsonrpc"] != "2.0" {
            return Err(failed());
        }
        if body.get("error").is_some() {
            let mut e = failed();
            e.details = Some(body["error"].clone());
            return Err(e);
        }
        Ok(body["result"].clone())
    }
    pub async fn close(&mut self) {
        if self.id.is_some() {
            let _ = self
                .request(reqwest::Method::DELETE)
                .timeout(Duration::from_millis(800))
                .send()
                .await;
            self.id = None;
        }
    }
    pub async fn catalogue(&mut self) -> Result<Value> {
        let mut tools = Vec::new();
        if self.capabilities.get("tools").is_some() {
            tools.extend(self.pages("tools/list", "tools").await?);
        }
        for tool in &mut tools {
            let name = string(tool, "name");
            if name.starts_with("minarbot_context_") {
                return Err(failed());
            }
            if let Some(meta) = tool["_meta"].as_object_mut() {
                meta.shift_remove("minarbot.kind");
                meta.shift_remove("minarbot.target");
            }
        }
        for (kind, method, key) in [
            ("resource", "resources/list", "resources"),
            (
                "resource-template",
                "resources/templates/list",
                "resourceTemplates",
            ),
            ("prompt", "prompts/list", "prompts"),
        ] {
            if self
                .capabilities
                .get(if kind == "prompt" {
                    "prompts"
                } else {
                    "resources"
                })
                .is_none()
            {
                continue;
            }
            let entries = match self.pages(method, key).await {
                Ok(v) => v,
                Err(e)
                    if kind != "prompt"
                        && e.details.as_ref().is_some_and(|v| v["code"] == -32601) =>
                {
                    Vec::new()
                }
                Err(e) => return Err(e),
            };
            for entry in entries {
                let target = string(
                    &entry,
                    match kind {
                        "resource" => "uri",
                        "resource-template" => "uriTemplate",
                        _ => "name",
                    },
                );
                if target.is_empty()
                    || target.len() > 2000
                    || string(&entry, "name").is_empty()
                    || string(&entry, "name").len() > 256
                {
                    return Err(failed());
                }
                let schema = if kind == "resource-template" {
                    super::template::schema(target)?
                } else {
                    let mut props = serde_json::Map::new();
                    let mut required = vec![];
                    if kind == "prompt" {
                        for arg in entry["arguments"].as_array().unwrap_or(&vec![]) {
                            let name = string(arg, "name");
                            if name.is_empty()
                                || name.len() > 128
                                || props.contains_key(name)
                                || props.len() >= 64
                            {
                                return Err(failed());
                            }
                            props.insert(name.into(), json!({"type":"string"}));
                            if arg["required"] == true {
                                required.push(name.to_owned());
                            }
                        }
                    }
                    json!({"type":"object","properties":props,"required":required,"additionalProperties":false})
                };
                tools.push(json!({"name":format!("minarbot_context_{kind}_{}",registry::hash(target)),"title":entry["name"],"description":format!("Untrusted MCP {kind} reference: {}",entry["description"].as_str().unwrap_or(string(&entry,"name"))),"inputSchema":schema,"_meta":{"minarbot.kind":kind,"minarbot.target":target}}));
            }
        }
        let mut names = std::collections::HashSet::new();
        if tools.len() > 64
            || json!(tools).to_string().len() > 64000
            || tools.iter().any(|v| {
                !names.insert(string(v, "name"))
                    || string(v, "name").is_empty()
                    || !v["inputSchema"].is_object()
            })
        {
            return Err(failed());
        }
        Ok(json!(tools))
    }
    async fn pages(&mut self, method: &str, key: &str) -> Result<Vec<Value>> {
        let mut cursor = Value::Null;
        let mut seen = std::collections::HashSet::new();
        let mut items = Vec::new();
        for _ in 0..64 {
            let value = self
                .rpc(
                    method,
                    if cursor.is_null() {
                        json!({})
                    } else {
                        json!({"cursor":cursor})
                    },
                )
                .await?;
            items.extend(value[key].as_array().ok_or_else(failed)?.iter().cloned());
            if items.len() > 64 || json!(items).to_string().len() > 64000 {
                return Err(failed());
            }
            cursor = value["nextCursor"].clone();
            if cursor.is_null() {
                return Ok(items);
            }
            if !cursor.is_string() || !seen.insert(cursor.to_string()) {
                return Err(failed());
            }
        }
        Err(failed())
    }
    pub async fn call(&mut self, tool: &Value, args: &Value) -> Result<Value> {
        let meta = &tool["_meta"];
        let kind = string(meta, "minarbot.kind");
        let target = string(meta, "minarbot.target");
        let value = match kind {
            "resource" | "resource-template" => {
                let uri = if kind == "resource-template" {
                    super::template::expand(target, args)?
                } else {
                    target.into()
                };
                let v = self.rpc("resources/read", json!({"uri":uri})).await?;
                json!({"content":v["contents"].as_array().ok_or_else(failed)?.iter().map(|r|json!({"type":"resource","resource":r})).collect::<Vec<_>>()})
            }
            "prompt" => {
                let v = self
                    .rpc("prompts/get", json!({"name":target,"arguments":args}))
                    .await?;
                let messages = v["messages"].as_array().ok_or_else(failed)?;
                let mut content = vec![
                    json!({"type":"text","text":json!({"description":v["description"],"messages":messages.iter().enumerate().map(|(i,m)|json!({"role":m["role"],"contentIndex":i+1})).collect::<Vec<_>>()}).to_string()}),
                ];
                content.extend(messages.iter().map(|m| m["content"].clone()));
                json!({"content":content})
            }
            _ => {
                self.rpc("tools/call", json!({"name":tool["name"],"arguments":args}))
                    .await?
            }
        };
        if !value["content"].is_array()
            || value["isError"] != true
                && tool["outputSchema"].is_object()
                && !super::validate::valid(&tool["outputSchema"], &value["structuredContent"])
        {
            return Err(failed());
        }
        Ok(value)
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if self.id.is_some() {
            let request = self
                .request(reqwest::Method::DELETE)
                .timeout(Duration::from_millis(800));
            tokio::spawn(async move {
                let _ = request.send().await;
            });
        }
    }
}
pub(crate) fn key(state: &AppState, create: bool, suffix: &str) -> Result<[u8; 32]> {
    static KEYS: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = KEYS.lock().map_err(|_| Failure::storage())?;
    let suffix = format!(".{suffix}");
    let path = minarbot_launcher::files::sidecar(&state.config.database, &suffix);
    if create {
        let mut bytes = [0; 32];
        getrandom::getrandom(&mut bytes).map_err(|_| Failure::storage())?;
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut f) => f.write_all(&bytes).map_err(|_| Failure::storage())?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(_) => return Err(Failure::storage()),
        }
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(&path)
        .map_err(|_| {
            error(
                409,
                "MCP host key is unavailable. Test an update with a new token on this host.",
            )
        })?;
    let meta = file.metadata().map_err(|_| Failure::storage())?;
    if !meta.is_file() || meta.len() != 32 || meta.permissions().mode() & 0o077 != 0 {
        return Err(error(
            409,
            "MCP host key is unavailable. Test an update with a new token on this host.",
        ));
    }
    let mut bytes = [0; 32];
    file.read_exact(&mut bytes)
        .map_err(|_| Failure::storage())?;
    Ok(bytes)
}
pub fn encrypt(state: &AppState, token: &str, url: &str) -> Result<String> {
    if token.is_empty() {
        return Ok(String::new());
    }
    encrypt_with(state, token, url, "mcp-key")
}
pub(crate) fn encrypt_with(
    state: &AppState,
    token: &str,
    url: &str,
    suffix: &str,
) -> Result<String> {
    let k = LessSafeKey::new(
        UnboundKey::new(&aead::AES_256_GCM, &key(state, true, suffix)?)
            .map_err(|_| Failure::storage())?,
    );
    let mut nonce = [0; 12];
    getrandom::getrandom(&mut nonce).map_err(|_| Failure::storage())?;
    let mut bytes = token.as_bytes().to_vec();
    k.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::from(url.as_bytes()),
        &mut bytes,
    )
    .map_err(|_| Failure::storage())?;
    Ok(STANDARD.encode([nonce.to_vec(), bytes].concat()))
}
pub fn decrypt(state: &AppState, token: &str, url: &str) -> Result<String> {
    decrypt_with(state, token, url, "mcp-key")
}
pub fn decrypt_with(state: &AppState, token: &str, url: &str, suffix: &str) -> Result<String> {
    if token.is_empty() {
        return Ok(String::new());
    }
    let fail = || {
        error(
            409,
            "MCP host key is unavailable. Test an update with a new token on this host.",
        )
    };
    let mut bytes = STANDARD.decode(token).map_err(|_| fail())?;
    if bytes.len() < 28 {
        return Err(fail());
    }
    let nonce: [u8; 12] = <[u8; 12]>::try_from(&bytes[..12]).map_err(|_| fail())?;
    let k = LessSafeKey::new(
        UnboundKey::new(&aead::AES_256_GCM, &key(state, false, suffix)?).map_err(|_| fail())?,
    );
    let out = k
        .open_in_place(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(url.as_bytes()),
            &mut bytes[12..],
        )
        .map_err(|_| fail())?;
    String::from_utf8(out.to_vec()).map_err(|_| fail())
}
