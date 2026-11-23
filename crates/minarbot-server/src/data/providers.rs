use super::*;
use crate::harness::model_catalog::{self, MemoryPricing};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Duration,
};

const DEFAULT_TURN_IDLE_TIMEOUT_MS: u64 = 10 * 60 * 1000;
const DEFAULT_TURN_MAX_MS: u64 = 60 * 60 * 1000;
const MIN_TURN_TIMEOUT_MS: u64 = 1;
const MAX_TURN_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct Options {
    #[serde(rename = "baseURL")]
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub default_model: Option<String>,
    pub provider: Option<String>,
    #[serde(default, deserialize_with = "account_values")]
    accounts: Option<Value>,
    #[serde(skip)]
    validated_accounts: Vec<Account>,
    pub timeout: Option<u64>,
    #[serde(skip)]
    pub turn_idle_timeout_ms: Option<u64>,
    #[serde(skip)]
    pub turn_max_ms: Option<u64>,
    pub concurrency: Option<Value>,
    pub project_root: Option<String>,
    pub stream: Option<bool>,
    pub stream_termination: Option<String>,
    pub max_tokens: Option<Value>,
}
fn account_values<'de, D: serde::Deserializer<'de>>(
    input: D,
) -> std::result::Result<Option<Value>, D::Error> {
    Value::deserialize(input).map(Some)
}
#[derive(Clone, PartialEq, Eq)]
pub struct Account {
    pub id: String,
    pub name: String,
    pub family: String,
    pub base: String,
    pub key: Option<String>,
    pub model: String,
    pub stream: bool,
    pub termination: String,
    pub max_tokens: u64,
    pub source: &'static str,
}
pub(super) fn supported_family(value: &str) -> bool {
    matches!(value, "openai-compatible" | "anthropic" | "gemini")
}
fn family(value: Option<&str>) -> crate::db::Result<String> {
    let value = value.unwrap_or("openai-compatible");
    if supported_family(value) {
        Ok(value.to_owned())
    } else {
        Err("MINARBOT_PROVIDER must be openai-compatible, anthropic or gemini".into())
    }
}
pub fn address(base: &str) -> crate::db::Result<url::Url> {
    let url = url::Url::parse(&format!("{}/models",base.strip_suffix('/').unwrap_or(base)))
        .map_err(|_| "Invalid provider address. Use HTTPS or loopback HTTP without credentials or a query in the URL.")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !(url.scheme() == "https"
            || url.scheme() == "http"
                && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
    {
        return Err("Invalid provider address. Use HTTPS or loopback HTTP without credentials or a query in the URL.".into());
    }
    Ok(url)
}
fn parse_accounts_environment(
    raw: std::result::Result<String, std::env::VarError>,
) -> crate::db::Result<Option<Value>> {
    match raw {
        Ok(raw) if raw.len() > 65536 => Err("Provider account configuration exceeds 64 KiB".into()),
        Ok(raw) => serde_json::from_str(&raw)
            .map(Some)
            .map_err(|_| "Provider account configuration must be valid JSON".into()),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => Err("Provider account configuration must be valid JSON".into()),
    }
}
fn timeout_environment(name: &str) -> crate::db::Result<Option<u64>> {
    match std::env::var(name) {
        Err(std::env::VarError::NotPresent) => Ok(None),
        raw => raw.ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| (MIN_TURN_TIMEOUT_MS..=MAX_TURN_TIMEOUT_MS).contains(value))
            .map(Some)
            .ok_or_else(|| format!("{name} must be an integer between {MIN_TURN_TIMEOUT_MS} and {MAX_TURN_TIMEOUT_MS} milliseconds").into()),
    }
}
impl Options {
    pub fn turn_idle_timeout(&self) -> Duration {
        Duration::from_millis(
            self.turn_idle_timeout_ms
                .unwrap_or(DEFAULT_TURN_IDLE_TIMEOUT_MS),
        )
    }

    pub fn turn_max_timeout(&self) -> Duration {
        Duration::from_millis(self.turn_max_ms.unwrap_or(DEFAULT_TURN_MAX_MS))
    }

    pub fn environment() -> crate::db::Result<Self> {
        use std::env;
        let provider = std::env::var("MINARBOT_PROVIDER").ok();
        let family = provider.as_deref().unwrap_or("openai-compatible");
        let prefix = match family {
            "anthropic" => "ANTHROPIC",
            "gemini" => "GEMINI",
            _ => "OPENAI",
        };
        let parsed = |key: &str| -> crate::db::Result<Option<Value>> {
            std::env::var(key)
                .ok()
                .map(|s| {
                    if s.len() > 65536 {
                        return Err(format!("{key} exceeds 64 KiB").into());
                    }
                    serde_json::from_str(&s).map_err(|_| format!("Invalid {key} JSON").into())
                })
                .transpose()
        };
        Ok(Self {
            base_url: env::var(format!("{prefix}_BASE_URL")).ok(),
            api_key: env::var(format!("{prefix}_API_KEY")).ok(),
            default_model: env::var(format!("{prefix}_MODEL")).ok(),
            provider,
            accounts: parse_accounts_environment(std::env::var("MINARBOT_PROVIDER_ACCOUNTS"))?,
            project_root: std::env::var("MINARBOT_PROJECT_ROOT").ok(),
            concurrency: parsed("MINARBOT_CONCURRENCY")?,
            stream: Some(std::env::var("MINARBOT_STREAM").as_deref() == Ok("1")),
            stream_termination: std::env::var("MINARBOT_STREAM_TERMINATION").ok(),
            max_tokens: parsed("MINARBOT_MAX_TOKENS")?,
            turn_idle_timeout_ms: timeout_environment("MINARBOT_TURN_IDLE_TIMEOUT_MS")?,
            turn_max_ms: timeout_environment("MINARBOT_TURN_MAX_MS")?,
            ..Self::default()
        })
    }
    pub fn validate(&mut self) -> crate::db::Result<()> {
        self.validated_accounts = self.parse_accounts()?;
        self.accounts = None;

        if self
            .concurrency
            .as_ref()
            .is_some_and(|v| !integer(v, 1, 32))
        {
            return Err("MINARBOT_CONCURRENCY must be 1..32".into());
        }
        if self
            .max_tokens
            .as_ref()
            .is_some_and(|v| !integer(v, 1, 64000))
        {
            return Err("MINARBOT_MAX_TOKENS must be 1..64000".into());
        }
        if self
            .stream_termination
            .as_ref()
            .is_some_and(|s| !["marker", "finish-reason"].contains(&s.as_str()))
        {
            return Err("MINARBOT_STREAM_TERMINATION must be marker or finish-reason".into());
        }
        if self.provider.as_deref().unwrap_or("openai-compatible") != "openai-compatible"
            && self.stream_termination.as_deref() == Some("finish-reason")
        {
            return Err(
                "finish-reason termination is only supported for OpenAI-compatible streams".into(),
            );
        }
        Ok(())
    }
    fn environment_accounts(&self) -> impl Iterator<Item = &Account> {
        self.validated_accounts.iter()
    }

    pub(crate) fn account(&self, id: &str) -> Option<Account> {
        self.environment_accounts()
            .find(|account| account.id == id)
            .cloned()
    }

    fn parse_accounts(&self) -> crate::db::Result<Vec<Account>> {
        let mut out = vec![Account {
            id: String::new(),
            name: "Default server".into(),
            family: family(self.provider.as_deref())?,
            base: self.base_url.clone().unwrap_or_default(),
            key: self.api_key.clone(),
            model: self.default_model.clone().unwrap_or_default(),
            stream: self.stream.unwrap_or(false),
            termination: self
                .stream_termination
                .clone()
                .unwrap_or_else(|| "marker".into()),
            max_tokens: self
                .max_tokens
                .as_ref()
                .and_then(Value::as_u64)
                .unwrap_or(4096),
            source: "environment",
        }];
        if let Some(base) = &self.base_url {
            address(base)?;
        }
        let Some(values) = &self.accounts else {
            return Ok(out);
        };
        let values = values
            .as_array()
            .filter(|v| v.len() <= 32)
            .ok_or("Provider accounts must be an array of at most 32 entries")?;
        let mut ids = BTreeSet::new();
        for v in values {
            if !closed(
                v,
                &[
                    "id",
                    "name",
                    "provider",
                    "baseURL",
                    "apiKey",
                    "defaultModel",
                    "stream",
                    "streamTermination",
                    "maxTokens",
                ],
            ) {
                return Err("Invalid provider account".into());
            }
            let id = string(v, "id");
            if id.is_empty()
                || id.len() > 64
                || !id
                    .bytes()
                    .next()
                    .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
                || !ids.insert(id)
            {
                return Err("Invalid or duplicate provider account ID".into());
            }
            let name = v["name"]
                .as_str()
                .filter(|s| {
                    !s.trim().is_empty()
                        && s.encode_utf16().count() <= 80
                        && !s.chars().any(|c| c < ' ' || c == '\u{7f}')
                })
                .ok_or("Invalid provider account name")?;
            let base = v["baseURL"]
                .as_str()
                .filter(|s| s.encode_utf16().count() <= 2048)
                .ok_or("Invalid provider account URL")?;
            address(base).map_err(|_| "Invalid provider account URL")?;
            for (key, max) in [("apiKey", 8192), ("defaultModel", 160)] {
                if v.get(key).is_some_and(|v| {
                    v.as_str().is_none_or(|s| {
                        s.encode_utf16().count() > max
                            || s.chars().any(|c| c < ' ' || c == '\u{7f}')
                    })
                }) {
                    return Err(format!("Invalid provider account {key}").into());
                }
            }
            if v.get("provider").is_some_and(|v| !v.is_string()) {
                return Err(
                    "MINARBOT_PROVIDER must be openai-compatible, anthropic or gemini".into(),
                );
            }
            if v.get("streamTermination").is_some_and(|v| !v.is_string()) {
                return Err("MINARBOT_STREAM_TERMINATION must be marker or finish-reason".into());
            }
            let family = family(v.get("provider").and_then(Value::as_str))?;
            let termination = v
                .get("streamTermination")
                .and_then(Value::as_str)
                .unwrap_or("marker");
            if !["marker", "finish-reason"].contains(&termination) {
                return Err("MINARBOT_STREAM_TERMINATION must be marker or finish-reason".into());
            }
            if family != "openai-compatible" && termination != "marker" {
                return Err("finish-reason termination requires OpenAI-compatible provider".into());
            }
            if v.get("stream").is_some_and(|v| !v.is_boolean()) {
                return Err("Invalid provider account streaming".into());
            }
            if v.get("maxTokens").is_some_and(|v| !integer(v, 1, 64000)) {
                return Err("Invalid provider account token limit".into());
            }
            out.push(Account {
                id: id.into(),
                name: name.trim().into(),
                family,
                base: base.into(),
                key: v["apiKey"].as_str().map(str::to_owned),
                model: string(v, "defaultModel").into(),
                stream: v["stream"].as_bool().unwrap_or(false),
                termination: termination.into(),
                max_tokens: v["maxTokens"].as_u64().unwrap_or(4096),
                source: "environment",
            });
        }
        Ok(out)
    }
}
pub fn accounts(state: &AppState) -> Result<Vec<Account>> {
    accounts_at(&state.config.data, Some(&state.config.database))
}

pub(crate) fn accounts_at(options: &Options, database: Option<&Path>) -> Result<Vec<Account>> {
    let mut accounts = options.environment_accounts().cloned().collect::<Vec<_>>();
    if let Some(database) = database {
        accounts.extend(super::provider_store::stored(database, options)?);
    }
    Ok(accounts)
}

fn host(base: &str) -> Option<String> {
    let url = url::Url::parse(base).ok()?;
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

const METADATA_ACCOUNTS: usize = 64;
const METADATA_BYTES: usize = 32 * 1024;

pub(crate) fn metadata(accounts: &[Account]) -> (Value, bool) {
    let mut entries = Vec::new();
    let mut ids = BTreeSet::new();
    let mut bytes = 2;
    for account in accounts {
        let Some(host) = host(&account.base) else {
            continue;
        };
        let fields = [
            &account.id,
            &account.name,
            &account.family,
            &host,
            &account.model,
        ];
        let entry = json!({"id":account.id,"name":account.name,"provider":account.family,
            "host":host,"defaultModel":account.model});
        if accounts
            .iter()
            .filter_map(|a| a.key.as_deref())
            .any(|key| !key.is_empty() && fields.iter().any(|value| value.contains(key)))
            || !valid_metadata_entry(&entry)
            || !ids.insert(&account.id)
        {
            continue;
        }
        bytes += entry.to_string().len() + 1;
        if entries.len() == METADATA_ACCOUNTS || bytes > METADATA_BYTES {
            break;
        }
        entries.push(entry);
    }
    let complete = entries.len()
        == accounts
            .iter()
            .filter(|account| !account.base.is_empty())
            .count();
    (json!(entries), complete)
}

fn valid_metadata_entry(entry: &Value) -> bool {
    if !closed(entry, &["id", "name", "provider", "host", "defaultModel"]) {
        return false;
    }
    let Some(id) = entry["id"].as_str() else {
        return false;
    };
    let Some(host) = entry["host"]
        .as_str()
        .filter(|host| (1..=259).contains(&host.len()))
    else {
        return false;
    };
    let valid_host = ["https", "http"].into_iter().any(|scheme| {
        let origin = format!("{scheme}://{host}");
        url::Url::parse(&origin).is_ok_and(|url| url.origin().ascii_serialization() == origin)
    });
    id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        && id
            .bytes()
            .next()
            .is_none_or(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && supported_family(string(entry, "provider"))
        && valid_host
        && [("name", 80), ("defaultModel", 160)]
            .iter()
            .all(|&(field, limit)| {
                entry[field].as_str().is_some_and(|value| {
                    (field != "name" || !value.trim().is_empty())
                        && value.encode_utf16().count() <= limit
                        && !value.chars().any(char::is_control)
                })
            })
}

pub(crate) fn validate_metadata(value: &Value) -> Result<()> {
    let mut ids = BTreeSet::new();
    if value.as_array().is_some_and(|entries| {
        entries.len() <= METADATA_ACCOUNTS
            && entries
                .iter()
                .all(|entry| valid_metadata_entry(entry) && ids.insert(string(entry, "id")))
            && value.to_string().len() <= METADATA_BYTES
    }) {
        return Ok(());
    }
    Err(Failure::new(
        400,
        "invalid_provider_accounts",
        "Invalid device provider accounts.",
    ))
}

pub(super) fn public(account: &Account) -> Value {
    let host = host(&account.base).unwrap_or_default();
    let mut result = json!({
        "id":account.id,"name":account.name,"provider":account.family,"host":host,
        "configured":!account.base.is_empty(),"defaultModel":account.model,
        "source":account.source,"hasKey":account.key.as_ref().is_some_and(|key| !key.is_empty())
    });
    if account.source == "settings" {
        result["baseURL"] = json!(account.base);
    }
    result
}
pub fn list(state: &AppState) -> Result<Value> {
    let mut accounts = state
        .config
        .data
        .environment_accounts()
        .map(public)
        .collect::<Vec<_>>();
    accounts.extend(super::provider_store::list(state)?);
    Ok(json!(accounts))
}
const INVALID: &str =
    "The provider returned an invalid model catalog. Check the /models endpoint response.";
const TIMEOUT: &str = "Model discovery timed out. Check the provider and try again.";
pub async fn models(state: &AppState, q: &[(String, String)]) -> Result<Value> {
    let account_id = param(q, "account").unwrap_or("");
    let runtime = param(q, "runtime");
    if let Some(engine @ ("codex" | "claude")) = runtime {
        if account_id == logins::DEVICE || account_id.starts_with(logins::PREFIX) {
            return crate::harness::model_catalog::models(state, engine, account_id, true).await;
        }
    }
    let account=accounts(state)?.into_iter().find(|a|a.id==account_id).ok_or_else(||error(503,"The selected provider account is missing. Choose another account or update server configuration."))?;
    if account.base.is_empty() {
        return Err(error(
            503,
            if account_id.is_empty() {
                "The default provider account is not configured. Check server configuration before trying model discovery."
            } else {
                "The selected provider account is not configured. Check server configuration or choose another account."
            },
        ));
    }
    if matches!(runtime, Some("api" | "codex" | "claude")) {
        model_catalog::api_models(state, &account).await
    } else {
        Ok(json!({"models": discover_with_timeout(state, &account, true).await?}))
    }
}

pub(super) async fn test_models(state: &AppState, account: &Account) -> Result<Vec<String>> {
    discover_with_timeout(state, account, false).await
}

async fn discover_with_timeout(
    state: &AppState,
    account: &Account,
    paginate: bool,
) -> Result<Vec<String>> {
    Ok(discover_catalog_with_timeout(state, account, paginate)
        .await?
        .into_keys()
        .collect())
}

pub(crate) async fn discover_catalog_with_timeout(
    state: &AppState,
    account: &Account,
    paginate: bool,
) -> Result<BTreeMap<String, Option<MemoryPricing>>> {
    tokio::time::timeout(
        Duration::from_millis(state.config.data.timeout.unwrap_or(15000).min(15000)),
        discover_catalog(account, paginate),
    )
    .await
    .map_err(|_| error(502, TIMEOUT))?
}
pub(crate) fn valid_model_id(id: &str) -> bool {
    !id.is_empty()
        && id.trim() == id
        && id.encode_utf16().count() <= 160
        && !id.chars().any(|c| c < ' ' || c == '\u{7f}')
}
pub(crate) async fn discover(a: &Account, paginate: bool) -> Result<Vec<String>> {
    Ok(discover_catalog(a, paginate).await?.into_keys().collect())
}

async fn discover_catalog(
    a: &Account,
    paginate: bool,
) -> Result<BTreeMap<String, Option<MemoryPricing>>> {
    let mut url = address(&a.base).map_err(|_| error(502, INVALID))?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| Failure::storage())?;
    let mut models = BTreeMap::new();
    let mut cursors = BTreeSet::new();
    let mut size = 0;
    for _ in 0..100 {
        let mut req = client.get(url.clone());
        if a.family == "anthropic" {
            req = req.header("anthropic-version", "2023-06-01");
        }
        if let Some(key) = &a.key {
            req = match a.family.as_str() {
                "gemini" => req.header("x-goog-api-key", key),
                "anthropic" => req.header("x-api-key", key),
                _ => req.bearer_auth(key),
            };
        }
        let mut response = req.send().await.map_err(|_| {
            error(
                502,
                "The model catalog host is unreachable. Check the provider URL and network.",
            )
        })?;
        let status = response.status().as_u16();
        let message=match status {300..=399=>Some("The model catalog redirected unexpectedly. Check the provider URL and /models endpoint."),401=>Some("The provider rejected the API key. Check the server's provider configuration."),403=>Some("The provider denied access to the model catalog. Check the API key permissions."),408=>Some(TIMEOUT),429=>Some("The provider rate limit blocked model discovery. Wait for quota to reset and try again."),500..=599=>Some("The provider is unavailable for model discovery. Check provider status and try again."),200..=299=>None,_=>Some(INVALID)};
        if let Some(m) = message {
            return Err(error(502, m));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| error(502, INVALID))? {
            size += chunk.len();
            if size > 2 * 1024 * 1024 {
                return Err(error(502, INVALID));
            }
            bytes.extend_from_slice(&chunk);
        }
        let v: Value = serde_json::from_slice(&bytes).map_err(|_| error(502, INVALID))?;
        if a.key.as_ref().is_some_and(|k| {
            !k.is_empty()
                && v.to_string()
                    .contains(&json!(k).to_string()[1..json!(k).to_string().len() - 1])
        }) {
            return Err(error(502, INVALID));
        }
        let rows = v[if a.family == "gemini" {
            "models"
        } else {
            "data"
        }]
        .as_array()
        .filter(|r| r.len() <= 10000)
        .ok_or_else(|| error(502, INVALID))?;
        for row in rows {
            if a.family == "gemini" {
                let name = row["name"]
                    .as_str()
                    .and_then(|s| s.strip_prefix("models/"))
                    .filter(|s| {
                        !s.is_empty()
                            && s.len() <= 151
                            && s.as_bytes()[0].is_ascii_alphanumeric()
                            && s.bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                    })
                    .ok_or_else(|| error(502, INVALID))?;
                let methods = row["supportedGenerationMethods"]
                    .as_array()
                    .ok_or_else(|| error(502, INVALID))?;
                if methods.iter().any(|v| v == "generateContent") {
                    models.insert(name.to_owned(), None);
                }
            } else {
                let id = row["id"]
                    .as_str()
                    .filter(|s| valid_model_id(s))
                    .ok_or_else(|| error(502, INVALID))?;
                models
                    .entry(id.to_owned())
                    .or_insert_with(|| MemoryPricing::from_row(row));
            }
        }
        if models.len() > 10000 {
            return Err(error(502, INVALID));
        }
        if !paginate {
            return Ok(models);
        }
        let cursor = if a.family == "gemini" {
            if v.get("nextPageToken").is_none() {
                return Ok(models);
            }
            v["nextPageToken"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 2000)
                .ok_or_else(|| error(502, INVALID))?
        } else if a.family == "anthropic" {
            let more = v["has_more"].as_bool().ok_or_else(|| error(502, INVALID))?;
            if !more {
                return Ok(models);
            }
            v["last_id"]
                .as_str()
                .filter(|s| rows.last().is_some_and(|r| r["id"] == *s))
                .ok_or_else(|| error(502, INVALID))?
        } else {
            return Ok(models);
        };
        if !cursors.insert(cursor.to_owned()) {
            return Err(error(502, INVALID));
        }
        url.query_pairs_mut().clear().append_pair(
            if a.family == "gemini" {
                "pageToken"
            } else {
                "after_id"
            },
            cursor,
        );
    }
    Err(error(502, INVALID))
}
pub fn usage(db: &Connection, q: &[(String, String)]) -> Result<Value> {
    let cursor = param(q, "before");
    let before = cursor
        .map(str::parse::<i64>)
        .transpose()
        .ok()
        .flatten()
        .unwrap_or(9_007_199_254_740_991);
    if q.iter().any(|(k, _)| k != "before")
        || cursor.is_some_and(|s| {
            s.is_empty()
                || s.starts_with('0')
                || !s.bytes().all(|b| b.is_ascii_digit())
                || s.parse::<i64>().is_err()
        })
        || before > 9_007_199_254_740_991
    {
        return Err(error(400, "Invalid usage position."));
    }
    let calls=all(db,"SELECT provider_calls.*,agents.name AS agent_name FROM provider_calls JOIN agents ON agents.id=provider_calls.agent WHERE seq<? ORDER BY seq DESC LIMIT 51",[before])?;
    let summary=one(db,"SELECT count(*) AS calls,count(*) FILTER(WHERE status='pending') AS pending,count(*) FILTER(WHERE status='failed') AS failed,count(*) FILTER(WHERE status='interrupted') AS interrupted,CAST(sum(input_tokens) AS TEXT) AS input_tokens,CAST(sum(output_tokens) AS TEXT) AS output_tokens,CAST(sum(total_tokens) AS TEXT) AS total_tokens,count(*) FILTER(WHERE input_tokens IS NULL) AS unknown_input,count(*) FILTER(WHERE output_tokens IS NULL) AS unknown_output,count(*) FILTER(WHERE total_tokens IS NULL) AS unknown_total FROM provider_calls",[])?.ok_or_else(Failure::storage)?;
    let transcription = if table(db, "transcriptions")? {
        one(db,"SELECT count(*) AS calls,count(*) FILTER(WHERE status='pending') AS pending,count(*) FILTER(WHERE status='uncertain') AS uncertain,count(*) FILTER(WHERE status='cancelled') AS cancelled,count(*) FILTER(WHERE usage IS NULL OR usage='null') AS unknown,CAST(sum(json_extract(usage,'$.input_tokens')) AS TEXT) AS input_tokens,CAST(sum(json_extract(usage,'$.output_tokens')) AS TEXT) AS output_tokens,CAST(sum(json_extract(usage,'$.total_tokens')) AS TEXT) AS total_tokens,sum(json_extract(usage,'$.seconds')) AS seconds FROM transcriptions",[])? .unwrap_or(json!({"calls":0,"pending":0,"uncertain":0,"cancelled":0,"unknown":0,"input_tokens":null,"output_tokens":null,"total_tokens":null,"seconds":null}))
    } else {
        json!({"calls":0,"pending":0,"uncertain":0,"cancelled":0,"unknown":0,"input_tokens":null,"output_tokens":null,"total_tokens":null,"seconds":null})
    };
    Ok(
        json!({"summary":summary,"next_before":if calls.len()>50{calls[49]["seq"].clone()}else{Value::Null},"calls":calls.into_iter().take(50).collect::<Vec<_>>(),"transcription":transcription}),
    )
}

pub(crate) fn format_timeout(timeout: Duration) -> String {
    let millis = timeout.as_millis();
    if millis > 0 && millis.is_multiple_of(60_000) {
        format!("{} min", millis / 60_000)
    } else if millis.is_multiple_of(1000) {
        format!("{} s", millis / 1000)
    } else {
        format!("{millis} ms")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_account_configuration_rejects_ambiguous_or_unsafe_entries() -> crate::db::Result<()>
    {
        let valid = json!({"id":"work","name":"Work","baseURL":"https://example.test/v1","provider":"openai-compatible"});
        let parse = |accounts: Option<Value>| {
            Options {
                accounts,
                ..Options::default()
            }
            .parse_accounts()
        };
        assert!(parse_accounts_environment(Err(std::env::VarError::NotPresent))?.is_none());
        let defaults = parse(None)?;
        assert_eq!(defaults.len(), 1);
        assert_eq!(defaults[0].family, "openai-compatible");
        assert_eq!(defaults[0].termination, "marker");
        assert!(family(Some("typo")).is_err());
        for base in [
            "http://public.example/v1",
            "https://key:secret@example.com/v1",
            "https://example.com/v1?key=secret",
        ] {
            assert!(address(base).is_err());
        }
        assert!(matches!(
            parse_accounts_environment(Ok("{secret-private-key".into())),
            Err(error) if !error.to_string().contains("secret-private-key")
        ));
        assert!(matches!(
            parse_accounts_environment(Ok(" ".repeat(65537))),
            Err(error) if error.to_string().contains("64 KiB")
        ));
        let parsed = parse(parse_accounts_environment(Ok(json!([valid]).to_string()))?)?;
        assert_eq!(parsed[1].id, "work");
        for accounts in [Value::Null, json!({}), json!([valid, valid])] {
            assert!(parse(Some(accounts)).is_err());
        }
        for (key, value) in [
            ("id", json!("")),
            ("apiKey", json!("x\nsecret")),
            ("baseURL", json!("http://example.test")),
            ("baseURL", json!("https://user:secret@example.test")),
            ("provider", json!("other")),
            ("extra", json!(true)),
            ("stream", json!("1")),
            ("maxTokens", json!(0)),
        ] {
            let mut account = valid.clone();
            account[key] = value;
            assert!(parse(Some(json!([account]))).is_err());
        }
        let mut gemini = valid;
        gemini["provider"] = json!("gemini");
        gemini["streamTermination"] = json!("finish-reason");
        assert!(parse(Some(json!([gemini]))).is_err());
        Ok(())
    }
}

#[cfg(test)]
mod device_metadata_tests {
    use super::*;

    #[test]
    fn device_inventory_rejects_invalid_and_oversized_metadata() -> Result<()> {
        let entry = json!({"id":"account","name":"Device","provider":"openai-compatible",
            "host":"[::1]:4321","defaultModel":"fixture"});
        validate_metadata(&json!([entry]))?;
        for (field, value) in [
            ("host", json!("fixture.example/private")),
            ("host", json!("user:secret@fixture.example")),
            ("host", json!("fixture.example?token=secret")),
            ("apiKey", json!("secret")),
            ("defaultModel", json!(42)),
            ("provider", json!("other")),
        ] {
            let mut invalid = entry.clone();
            invalid[field] = value;
            assert!(validate_metadata(&json!([invalid])).is_err(), "{field}");
        }
        let too_many = (0..=METADATA_ACCOUNTS)
            .map(|n| {
                let mut account = entry.clone();
                account["id"] = json!(n.to_string());
                account
            })
            .collect::<Vec<_>>();
        assert!(validate_metadata(&json!(too_many)).is_err());
        let label = "h".repeat(60);
        let host = [label.as_str(); 4].join(".");
        let oversized = (0..METADATA_ACCOUNTS)
            .map(|n| {
                json!({
                    "id": format!("account-{n}"),
                    "name": "n".repeat(80),
                    "provider": "openai-compatible",
                    "host": format!("{host}:{}", 4000 + n),
                    "defaultModel": "m".repeat(160)
                })
            })
            .collect::<Vec<_>>();
        assert!(oversized.iter().all(valid_metadata_entry));
        assert!(validate_metadata(&json!(oversized)).is_err());
        assert!(validate_metadata(&json!([entry, entry])).is_err());
        Ok(())
    }
}
