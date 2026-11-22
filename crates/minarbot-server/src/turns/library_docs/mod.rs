mod sources;
mod stack;
#[cfg(test)]
mod tests;

use super::{closed, error, exists, string, AppState, Failure, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sources::Sources;
use stack::{name_matches, Ecosystem, Library};
use std::{path::PathBuf, sync::LazyLock, time::Duration};

const DAY: i64 = 86_400_000;
const HOUR: i64 = 3_600_000;
const MAX_TOPIC: usize = 96;
const MAX_STACK_ROWS: usize = 64;
const STACK_PREAMBLE: &str =
    "\n\nDetected project libraries (reference data, use library_docs for version-sensitive APIs): ";
static SCANS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
static SOURCES: LazyLock<Sources> = LazyLock::new(Sources::default);

pub(super) fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(include_str!("schema.sql"))?;
    Ok(())
}

pub(super) fn enabled(db: &Connection, agent: &str) -> Result<bool> {
    exists(
        db,
        "SELECT 1 FROM agents WHERE id=? AND web_access=1 AND library_docs_access=1",
        [agent],
    )
}

fn root(state: &AppState, db: &Connection, turn: &Value) -> Result<Option<PathBuf>> {
    Ok(super::tools::workspace(state, db, turn)?.map(|binding| binding.directory))
}

fn authorized(db: &Connection, turn: &Value) -> Result<()> {
    if !enabled(db, string(turn, "agent"))?
        || super::profile::setup(db, turn)?
        || crate::automation::model(db, string(turn, "id"))?.is_some()
        || !exists(
            db,
            "SELECT 1 FROM turns WHERE id=? AND agent=? AND status='running'",
            [string(turn, "id"), string(turn, "agent")],
        )?
    {
        return Err(error(403, "Library docs are unavailable for this turn."));
    }
    Ok(())
}

fn confirm_workspace(
    state: &AppState,
    db: &Connection,
    turn: &Value,
    expected: &Option<PathBuf>,
) -> Result<()> {
    authorized(db, turn)?;
    if &root(state, db, turn)? != expected {
        return Err(error(409, "Working folder changed. Repeat the lookup."));
    }
    Ok(())
}

async fn scan(root: Option<PathBuf>) -> Result<Vec<Library>> {
    let Some(root) = root else {
        return Ok(Vec::new());
    };
    let permit = SCANS.acquire().await.map_err(|_| Failure::storage())?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        stack::scan(&root)
    })
    .await
    .map_err(|_| Failure::storage())
}

pub(super) async fn summary(state: &AppState, turn: &Value) -> Result<String> {
    let root = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        if turn["agent"] == crate::mcp::guide::ID
            || !enabled(&guard.connection, string(turn, "agent"))?
            || super::profile::setup(&guard.connection, turn)?
            || crate::automation::model(&guard.connection, string(turn, "id"))?.is_some()
        {
            return Ok(String::new());
        }
        root(state, &guard.connection, turn)?
    };
    let libraries = scan(root).await?;
    let mut summary = String::new();
    for entry in libraries
        .iter()
        .filter(|entry| entry.version.is_some())
        .take(16)
    {
        let item = format!(
            "{}@{}",
            entry.name,
            entry.version.as_deref().unwrap_or_default()
        );
        if summary.len() + item.len() > 500 {
            break;
        }
        if summary.is_empty() {
            summary.push_str(STACK_PREAMBLE);
        } else {
            summary.push_str(", ");
        }
        summary.push_str(&item);
    }
    Ok(summary)
}

#[derive(Debug)]
struct Lookup {
    name: String,
    topic: String,
    version: Option<String>,
    ecosystem: Option<Ecosystem>,
}

impl Lookup {
    fn from_args(args: &Value) -> Result<Self> {
        let name = string(args, "library").trim();
        let topic = string(args, "topic").trim();
        if !closed(args, &["library", "topic", "version"]) || !stack::package_name(name) {
            return Err(error(400, "Enter a public library name."));
        }
        // A short phrase of public API identifiers/keywords, rather than a chat or code field.
        if topic.is_empty()
            || topic.len() > MAX_TOPIC
            || topic.split(' ').count() > 8
            || !topic
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b" _.-".contains(&b))
            || topic
                .split(' ')
                .any(|word| word.len() > 40 || word.contains("..") || word.starts_with(['.', '-']))
        {
            return Err(error(
                400,
                "Use a short public API topic, such as useEffect cleanup.",
            ));
        }
        let version = args
            .get("version")
            .map(|v| {
                v.as_str()
                    .and_then(stack::exact)
                    .map(str::to_owned)
                    .ok_or_else(|| error(400, "Use an exact library version."))
            })
            .transpose()?;
        Ok(Self {
            name: name.into(),
            topic: topic.split_ascii_whitespace().collect::<Vec<_>>().join(" "),
            version,
            ecosystem: None,
        })
    }

    fn resolve(&mut self, stack: &[Library]) -> Result<()> {
        let matching: Vec<_> = stack
            .iter()
            .filter(|entry| name_matches(entry, &self.name))
            .collect();
        let exact: Vec<_> = matching
            .iter()
            .copied()
            .filter(|entry| {
                self.version
                    .as_ref()
                    .is_some_and(|v| entry.version.as_ref() == Some(v))
            })
            .collect();
        let matching = if exact.is_empty() { &matching } else { &exact };
        if let Some(entry) = matching.first() {
            if matching
                .iter()
                .any(|other| other.ecosystem != entry.ecosystem)
            {
                return Err(error(409, "Library name matches multiple ecosystems. Use its full package name or exact version."));
            }
            self.name.clone_from(&entry.name);
            self.ecosystem = Some(entry.ecosystem);
            if self.version.is_none() {
                let mut versions = matching.iter().filter_map(|entry| entry.version.as_ref());
                if let Some(version) = versions.next() {
                    if versions.any(|other| other != version) {
                        return Err(error(409, "Project has multiple versions of this library. Specify a version from project_stack."));
                    }
                    self.version = Some(version.clone());
                }
            }
        } else {
            self.ecosystem = if self.name.starts_with('@') {
                Some(Ecosystem::Npm)
            } else if self.name.contains(':') {
                Some(Ecosystem::Maven)
            } else if self
                .name
                .split_once('/')
                .is_some_and(|(prefix, _)| prefix.contains('.'))
            {
                Some(Ecosystem::Go)
            } else if self.name.contains('/') {
                Some(Ecosystem::Php)
            } else {
                let mut ecosystems = stack.iter().map(|e| e.ecosystem);
                ecosystems
                    .next()
                    .filter(|first| ecosystems.all(|other| other == *first))
            };
        }
        Ok(())
    }

    fn fields(&self) -> [&str; 3] {
        [
            &self.name,
            &self.topic,
            self.version.as_deref().unwrap_or(""),
        ]
    }

    fn cache_key(&self) -> (String, &str, String) {
        let ecosystem = self
            .ecosystem
            .and_then(|e| serde_json::to_value(e).ok())
            .unwrap_or(Value::Null);
        (
            format!("{ecosystem}:{}", self.name),
            self.version.as_deref().unwrap_or(""),
            crate::db::digest(&self.topic.to_ascii_lowercase()),
        )
    }

    fn cached(&self, db: &Connection, now: i64) -> Result<Option<Value>> {
        let (name, version, topic) = self.cache_key();
        let cached: Option<String> = db.query_row("SELECT result FROM library_docs_cache WHERE library=? AND version=? AND topic=? AND expires>?",
            params![name, version, topic, now], |r| r.get(0)).optional()?;
        cached
            .map(|text| {
                let mut result: Value =
                    serde_json::from_str(&text).map_err(|_| Failure::storage())?;
                result["cached"] = json!(true);
                Ok(result)
            })
            .transpose()
    }

    fn store(&self, db: &Connection, docs: sources::Docs, now: i64) -> Result<Value> {
        let (name, version, topic) = self.cache_key();
        let expires = now + if docs.source == "Context7" { DAY } else { HOUR };
        let mut result = json!(docs);
        result["library"] = json!(self.name);
        result["version"] = json!(self.version);
        result["cached"] = json!(false);
        super::transaction(db, || {
            db.execute("DELETE FROM library_docs_cache WHERE expires<=?", [now])?;
            db.execute("INSERT INTO library_docs_cache VALUES(?,?,?,?,?) ON CONFLICT(library,version,topic) DO UPDATE SET result=excluded.result,expires=excluded.expires",
                params![name,version,topic,result.to_string(),expires])?;
            db.execute("DELETE FROM library_docs_cache WHERE rowid IN (SELECT rowid FROM library_docs_cache ORDER BY expires DESC LIMIT -1 OFFSET 512)", [])?;
            Ok(())
        })?;
        Ok(result)
    }
}

pub(super) async fn tool(
    state: &AppState,
    turn: &Value,
    name: &str,
    args: &Value,
) -> Result<Value> {
    run(state, turn, name, args, &SOURCES).await
}

async fn run(
    state: &AppState,
    turn: &Value,
    name: &str,
    args: &Value,
    sources: &Sources,
) -> Result<Value> {
    let mut lookup = match name {
        "project_stack"
            if closed(args, &["offset"])
                && args.get("offset").is_none_or(|v| v.as_u64().is_some()) =>
        {
            None
        }
        "library_docs" => Some(Lookup::from_args(args)?),
        _ => return Err(error(400, "Invalid project_stack arguments.")),
    };
    let workspace = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        authorized(&guard.connection, turn)?;
        root(state, &guard.connection, turn)?
    };
    let stack = scan(workspace.clone()).await?;
    let Some(lookup) = lookup.as_mut() else {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        confirm_workspace(state, &guard.connection, turn, &workspace)?;
        let offset = args
            .get("offset")
            .map_or(Some(0), |v| {
                v.as_u64().and_then(|n| usize::try_from(n).ok())
            })
            .ok_or_else(|| error(400, "Invalid project stack offset."))?
            .min(stack.len());
        let end = offset.saturating_add(MAX_STACK_ROWS).min(stack.len());
        return Ok(
            json!({"libraries":&stack[offset..end],"count":stack.len(),"next_offset":(end<stack.len()).then_some(end)}),
        );
    };
    lookup.resolve(&stack)?;
    let now = state.config.now();
    let (key, redactions) = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        confirm_workspace(state, db, turn, &workspace)?;
        let project = super::tools::project_context(db, turn)?;
        let key = crate::computer::vault::context7_key(state, db)
            .ok()
            .flatten();
        let mut redactions = crate::mcp::secrets::project(
            state,
            db,
            project.as_ref().and_then(|p| p["project"].as_str()),
        )?;
        redactions.extend(crate::computer::vault::redactions(state, db)?);
        if redactions.iter().any(|secret| {
            lookup
                .fields()
                .iter()
                .any(|part| crate::mcp::secrets::reflected(part.as_bytes(), secret))
        }) {
            return Err(error(
                400,
                "Documentation lookup may only contain public library details.",
            ));
        }
        if let Some(cached) = lookup.cached(db, now)? {
            return Ok(cached);
        }
        (key, redactions)
    };
    let mut docs = tokio::time::timeout(Duration::from_secs(20), async {
        // One concurrent Context7 lookup. Contending calls can still use official sources.
        if let Ok(_permit) = sources.request.try_lock() {
            if let Ok(Some(docs)) = tokio::time::timeout(
                Duration::from_secs(7),
                sources.context7(lookup, key.as_deref(), now),
            )
            .await
            {
                return Some(docs);
            }
        }
        sources.official(lookup).await
    })
    .await
    .ok()
    .flatten()
    .ok_or_else(|| {
        error(
            404,
            "No documentation found. Check the library name and version.",
        )
    })?;
    docs.content = crate::mcp::secrets::redact(&docs.content, &redactions, false);
    docs.url = crate::mcp::secrets::redact(&docs.url, &redactions, false);
    docs.source_version = docs
        .source_version
        .map(|version| crate::mcp::secrets::redact(&version, &redactions, false));
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    confirm_workspace(state, &guard.connection, turn, &workspace)?;
    if docs.source != "Context7" {
        if let Some(cached) = lookup.cached(&guard.connection, state.config.now())? {
            if cached["source"] == "Context7" {
                return Ok(cached);
            }
        }
    }
    lookup.store(&guard.connection, docs, state.config.now())
}

pub(crate) async fn settings(state: &AppState) -> Result<Value> {
    let (key_set, key) = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        let key_set = exists(
            db,
            "SELECT 1 FROM vault_entries WHERE name=? AND kind='secret'",
            [crate::computer::vault::CONTEXT7_KEY],
        )?;
        (
            key_set,
            crate::computer::vault::context7_key(state, db)
                .ok()
                .flatten(),
        )
    };
    let reachable = if let Ok(_permit) = SOURCES.request.try_lock() {
        SOURCES.reachable(key.as_deref(), state.config.now()).await
    } else {
        false
    };
    Ok(json!({"reachable":reachable,"keySet":key_set}))
}
