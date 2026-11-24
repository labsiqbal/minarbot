#![allow(dead_code)]

use super::*;
use crate::harness::model_catalog::MemoryPricing;
use serde::Deserialize;
use std::{collections::BTreeMap, time::Duration};

const DEFAULT_TURN_IDLE_TIMEOUT_MS: u64 = 10 * 60 * 1000;
const DEFAULT_TURN_MAX_MS: u64 = 60 * 60 * 1000;
const MIN_TURN_TIMEOUT_MS: u64 = 1;
const MAX_TURN_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Options {
    #[serde(rename = "baseURL")]
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub provider: Option<String>,
    #[serde(default)]
    #[serde(deserialize_with = "account_values")]
    pub accounts: Option<Value>,
    pub default_model: Option<String>,
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

pub(crate) fn supported_family(value: &str) -> bool {
    matches!(value, "openai-compatible" | "anthropic" | "gemini")
}

pub fn address(base: &str) -> crate::db::Result<url::Url> {
    let url = url::Url::parse(base)
        .map_err(|_| "Invalid provider instance address. Use an absolute URL.")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !(url.scheme() == "https"
            || url.scheme() == "http"
                && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
    {
        return Err("Invalid provider instance address.".into());
    }
    Ok(url)
}

fn timeout_environment(name: &str) -> crate::db::Result<Option<u64>> {
    match std::env::var(name) {
        Err(std::env::VarError::NotPresent) => Ok(None),
        raw => raw
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| (MIN_TURN_TIMEOUT_MS..=MAX_TURN_TIMEOUT_MS).contains(value))
            .map(Some)
            .ok_or_else(|| format!("{name} must be a valid timeout in milliseconds").into()),
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
        Ok(Self {
            base_url: None,
            api_key: None,
            provider: None,
            accounts: None,
            default_model: std::env::var("MINARBOT_MODEL").ok(),
            timeout: std::env::var("MINARBOT_TIMEOUT")
                .ok()
                .and_then(|value| value.parse().ok()),
            concurrency: None,
            project_root: std::env::var("MINARBOT_PROJECT_ROOT").ok(),
            stream: None,
            stream_termination: None,
            max_tokens: None,
            turn_idle_timeout_ms: timeout_environment("MINARBOT_TURN_IDLE_TIMEOUT_MS")?,
            turn_max_ms: timeout_environment("MINARBOT_TURN_MAX_MS")?,
        })
    }

    pub fn validate(&mut self) -> crate::db::Result<()> {
        if let Some(values) = &self.accounts {
            let values = values
                .as_array()
                .filter(|values| values.len() <= 32)
                .ok_or_else(|| {
                    "Provider accounts must be an array of at most 32 entries".to_owned()
                })?;
            for value in values {
                let base = value["baseURL"]
                    .as_str()
                    .ok_or_else(|| "Invalid provider account URL".to_owned())?;
                address(base).map_err(|_| "Invalid provider account URL".to_owned())?;
                if value
                    .get("provider")
                    .is_some_and(|provider| provider.as_str().is_none())
                {
                    return Err(
                        "MINARBOT_PROVIDER must be openai-compatible, anthropic or gemini".into(),
                    );
                }
                if value
                    .get("streamTermination")
                    .is_some_and(|termination| termination.as_str().is_none())
                {
                    return Err(
                        "MINARBOT_STREAM_TERMINATION must be marker or finish-reason".into(),
                    );
                }
            }
        }
        if self
            .concurrency
            .as_ref()
            .is_some_and(|value| !integer(value, 1, 32))
        {
            return Err("MINARBOT_CONCURRENCY must be 1..32".into());
        }
        if self
            .max_tokens
            .as_ref()
            .is_some_and(|value| !integer(value, 1, 64000))
        {
            return Err("MINARBOT_MAX_TOKENS must be 1..64000".into());
        }
        if self
            .stream_termination
            .as_ref()
            .is_some_and(|value| !["marker", "finish-reason"].contains(&value.as_str()))
        {
            return Err("MINARBOT_STREAM_TERMINATION must be marker or finish-reason".into());
        }
        if self.provider.as_deref() == Some("anthropic")
            && self.stream_termination.as_deref() == Some("finish-reason")
        {
            return Err(
                "finish-reason termination is only supported for OpenAI-compatible streams".into(),
            );
        }
        Ok(())
    }
}

pub(crate) fn valid_model_id(id: &str) -> bool {
    !id.is_empty()
        && id.trim() == id
        && id.encode_utf16().count() <= 160
        && !id.chars().any(|c| c < ' ' || c == '\u{7f}')
}

pub fn accounts(state: &AppState) -> Result<Vec<Account>> {
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    accounts_at(&guard.connection)
}

pub(crate) fn accounts_at(db: &Connection) -> Result<Vec<Account>> {
    Ok(all(
        db,
        "SELECT id,display_name,kind FROM provider_instances WHERE enabled=1 ORDER BY rowid",
        [],
    )?
    .into_iter()
    .map(|row| Account {
        id: string(&row, "id").to_owned(),
        name: string(&row, "display_name").to_owned(),
        family: string(&row, "kind").to_owned(),
        base: format!("instance://{}", string(&row, "id")),
        key: None,
        model: String::new(),
        stream: false,
        termination: "marker".into(),
        max_tokens: 4096,
        source: "instance",
    })
    .collect())
}

pub(crate) fn metadata(accounts: &[Account]) -> (Value, bool) {
    let values = accounts
        .iter()
        .filter(|account| !account.id.is_empty())
        .map(|account| {
            json!({
                "id": account.id,
                "name": account.name,
                "provider": account.family,
                "host": url::Url::parse(&account.base)
                    .ok()
                    .and_then(|url| url.host_str().map(str::to_owned))
                    .unwrap_or_default(),
                "defaultModel": account.model
            })
        })
        .collect::<Vec<_>>();
    (json!(values), true)
}

pub(crate) fn validate_metadata(value: &Value) -> Result<()> {
    if value.as_array().is_some_and(|values| values.len() <= 64) {
        Ok(())
    } else {
        Err(error(400, "Invalid provider instance metadata."))
    }
}

pub async fn models(_state: &AppState, _q: &[(String, String)]) -> Result<Value> {
    Err(error(404, "Use a provider instance model catalog."))
}

pub(crate) async fn api_models(_state: &AppState, _account: &Account) -> Result<Value> {
    Err(error(404, "Use a provider instance model catalog."))
}

pub(crate) async fn discover(_account: &Account, _paginate: bool) -> Result<Vec<String>> {
    Ok(Vec::new())
}

pub(crate) async fn discover_catalog_with_timeout(
    _state: &AppState,
    _account: &Account,
    _paginate: bool,
) -> Result<BTreeMap<String, Option<MemoryPricing>>> {
    Ok(BTreeMap::new())
}

pub fn usage(db: &Connection, q: &[(String, String)]) -> Result<Value> {
    let cursor = param(q, "before");
    let before = cursor
        .map(str::parse::<i64>)
        .transpose()
        .ok()
        .flatten()
        .unwrap_or(9_007_199_254_740_991);
    if q.iter().any(|(key, _)| key != "before")
        || cursor.is_some_and(|value| value.is_empty() || value.starts_with('0'))
    {
        return Err(error(400, "Invalid usage position."));
    }
    let calls = all(
        db,
        "SELECT provider_calls.*,agents.name AS agent_name FROM provider_calls JOIN agents ON agents.id=provider_calls.agent WHERE seq<? ORDER BY seq DESC LIMIT 51",
        [before],
    )?;
    let summary = one(
        db,
        "SELECT count(*) AS calls,count(*) FILTER(WHERE status='pending') AS pending,count(*) FILTER(WHERE status='failed') AS failed,count(*) FILTER(WHERE status='interrupted') AS interrupted,CAST(sum(input_tokens) AS TEXT) AS input_tokens,CAST(sum(output_tokens) AS TEXT) AS output_tokens,CAST(sum(total_tokens) AS TEXT) AS total_tokens,count(*) FILTER(WHERE input_tokens IS NULL) AS unknown_input,count(*) FILTER(WHERE output_tokens IS NULL) AS unknown_output,count(*) FILTER(WHERE total_tokens IS NULL) AS unknown_total FROM provider_calls",
        [],
    )?
    .ok_or_else(Failure::storage)?;
    Ok(json!({
        "summary": summary,
        "next_before": if calls.len() > 50 { calls[49]["seq"].clone() } else { Value::Null },
        "calls": calls.into_iter().take(50).collect::<Vec<_>>()
    }))
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
