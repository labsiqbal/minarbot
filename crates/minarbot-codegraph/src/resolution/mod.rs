//! Adapted from colbymchenry/codegraph v1.6.2 (6560052), src/resolution/index.ts
//! and src/db/queries.ts. License: MIT (Copyright (c) 2026 Colby Mchenry).

mod builtins;
mod config;
mod imports;
mod matcher;
mod synthesis;

use crate::scan;
use config::Config;
use imports::Module;
use rusqlite::{params, Connection};
use serde_json::json;
use std::{
    cell::OnceCell,
    collections::{HashMap, HashSet},
    path::Path,
    sync::Arc,
};

const RETRY_CEILING: usize = 500;
const BATCH: usize = 512;

#[derive(Default)]
pub(crate) struct Changes {
    names: HashSet<String>,
    added_files: bool,
    changed: bool,
}

impl Changes {
    pub(crate) fn removing(&mut self, db: &Connection, path: &str) -> rusqlite::Result<()> {
        self.changed = true;
        let mut statement =
            db.prepare_cached("SELECT DISTINCT name FROM nodes WHERE file_path = ? AND kind NOT IN ('file', 'import')")?;
        for name in statement.query_map([path], |row| row.get::<_, String>(0))? {
            self.names.insert(name?);
        }
        // Preserve the call site's spelling before the target's FK cascade (#1240).
        db.prepare_cached(
            "INSERT INTO unresolved_refs
             (from_node_id, reference_name, reference_kind, line, col, candidates, file_path, language)
             SELECT e.source, json_extract(e.metadata, '$.refName'),
                    coalesce(json_extract(e.metadata, '$.refKind'), e.kind), e.line, e.col,
                    json_extract(e.metadata, '$.refCandidates'), src.file_path, src.language
             FROM nodes dst JOIN edges e ON e.target = dst.id JOIN nodes src ON src.id = e.source
             WHERE dst.file_path = ?1 AND src.file_path <> ?1
               AND e.line IS NOT NULL AND e.col IS NOT NULL
               AND CASE WHEN json_valid(e.metadata) THEN json_extract(e.metadata, '$.refName') END IS NOT NULL",
        )?
        .execute([path])?;
        Ok(())
    }

    pub(crate) fn inserted(&mut self, source: &scan::Source, added: bool) {
        self.names.extend(
            source
                .graph
                .nodes
                .iter()
                .filter(|node| !matches!(node.kind, "file" | "import"))
                .map(|node| node.name.clone()),
        );
        self.added_files |= added;
        self.changed = true;
    }

    fn retry(&self, db: &Connection) -> rusqlite::Result<()> {
        let mut retry = db.prepare_cached(
            "UPDATE unresolved_refs SET status = 'pending'
             WHERE status = 'failed' AND name_tail = ?1
             AND (SELECT count(*) FROM (
                 SELECT 1 FROM unresolved_refs WHERE status = 'failed' AND name_tail = ?1 LIMIT ?2
             )) < ?2",
        )?;
        // A new same-named definition changes the candidate set of already
        // resolved references too (CG-33). Restore only resolution-owned edges.
        let mut restore = db.prepare_cached(
            "INSERT INTO unresolved_refs
             (from_node_id, reference_name, reference_kind, line, col, candidates, file_path, language)
             SELECT e.source, json_extract(e.metadata, '$.refName'),
                    coalesce(json_extract(e.metadata, '$.refKind'), e.kind), e.line, e.col,
                    json_extract(e.metadata, '$.refCandidates'), src.file_path, src.language
             FROM nodes dst JOIN edges e ON e.target = dst.id JOIN nodes src ON src.id = e.source
             WHERE dst.name = ?1
               AND e.line IS NOT NULL AND e.col IS NOT NULL
               AND CASE WHEN json_valid(e.metadata) THEN json_extract(e.metadata, '$.refName') END IS NOT NULL",
        )?;
        let mut remove = db.prepare_cached(
            "DELETE FROM edges WHERE target IN (SELECT id FROM nodes WHERE name = ?1)
               AND line IS NOT NULL AND col IS NOT NULL
               AND CASE WHEN json_valid(metadata) THEN json_extract(metadata, '$.refName') END IS NOT NULL",
        )?;
        for name in &self.names {
            retry.execute(params![name, RETRY_CEILING + 1])?;
            if restore.execute([name])? != 0 {
                remove.execute([name])?;
            }
        }
        // An extensionless module path has no terminal symbol name.
        if self.added_files {
            db.execute(
                "UPDATE unresolved_refs SET status = 'pending'
                 WHERE status = 'failed' AND reference_kind = 'imports'",
                [],
            )?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Symbol {
    id: String,
    kind: String,
    name: String,
    qualified: String,
    file: String,
    language: String,
    start: u32,
    end: u32,
    column: u32,
    end_column: u32,
    exported: bool,
    visibility: Option<String>,
    signature: Option<String>,
    return_type: Option<String>,
    scope: OnceCell<Option<(u32, u32)>>,
}

impl Symbol {
    fn owner(&self) -> Option<&str> {
        self.qualified.rsplit_once("::").map(|(owner, _)| owner)
    }
}

#[derive(Clone)]
struct Reference {
    row: i64,
    source: String,
    name: String,
    kind: String,
    line: u32,
    column: u32,
    file: String,
    language: String,
    candidates: Option<String>,
}

#[derive(Clone, Copy)]
struct Match {
    target: usize,
    confidence: f64,
    by: &'static str,
    framework: Option<&'static str>,
}

impl Match {
    fn new(target: usize, confidence: f64, by: &'static str) -> Self {
        Self {
            target,
            confidence,
            by,
            framework: None,
        }
    }
}

struct Resolver<'a> {
    root: &'a Path,
    symbols: Vec<Symbol>,
    ids: HashMap<String, usize>,
    names: HashMap<String, Vec<usize>>,
    qualified: HashMap<String, Vec<usize>>,
    files: HashMap<String, Vec<usize>>,
    modules: HashMap<String, (u64, Arc<Module>)>,
    module_clock: u64,
    supers: HashMap<usize, Vec<usize>>,
    config: Config,
    ceiling: usize,
}

impl<'a> Resolver<'a> {
    fn load(db: &Connection, root: &'a Path) -> rusqlite::Result<Self> {
        // ponytail: a loaded symbol index keeps matching decisions deterministic.
        // Move the name/file buckets behind bounded SQLite caches if large
        // project indexes need lower peak heap than one record per symbol.
        let mut resolver = Self {
            root,
            symbols: Vec::new(),
            ids: HashMap::new(),
            names: HashMap::new(),
            qualified: HashMap::new(),
            files: HashMap::new(),
            modules: HashMap::new(),
            module_clock: 0,
            supers: HashMap::new(),
            config: Config::default(),
            ceiling: std::env::var("CODEGRAPH_AMBIGUOUS_NAME_CEILING")
                .ok()
                .and_then(|value| value.parse().ok())
                .filter(|value| *value > 0)
                .unwrap_or(500),
        };
        let mut statement = db.prepare(
            "SELECT id, kind, name, qualified_name, file_path, language, start_line, end_line,
                    start_column, end_column, is_exported, visibility, signature, return_type
             FROM nodes ORDER BY file_path, start_line, id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Symbol {
                id: row.get(0)?,
                kind: row.get(1)?,
                name: row.get(2)?,
                qualified: row.get(3)?,
                file: row.get(4)?,
                language: row.get(5)?,
                start: row.get(6)?,
                end: row.get(7)?,
                column: row.get(8)?,
                end_column: row.get(9)?,
                exported: row.get(10)?,
                visibility: row.get(11)?,
                signature: row.get(12)?,
                return_type: row.get(13)?,
                scope: OnceCell::new(),
            })
        })?;
        for row in rows {
            let symbol = row?;
            let index = resolver.symbols.len();
            resolver.ids.insert(symbol.id.clone(), index);
            resolver
                .names
                .entry(symbol.name.clone())
                .or_default()
                .push(index);
            resolver
                .qualified
                .entry(symbol.qualified.clone())
                .or_default()
                .push(index);
            resolver
                .files
                .entry(symbol.file.clone())
                .or_default()
                .push(index);
            resolver.symbols.push(symbol);
        }
        resolver.config = Config::load(root, resolver.files.keys().map(String::as_str));
        let mut statement = db.prepare(
            "SELECT source, target FROM edges WHERE kind IN ('extends', 'implements') ORDER BY id",
        )?;
        for edge in statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (source, target) = edge?;
            if let (Some(&source), Some(&target)) =
                (resolver.ids.get(&source), resolver.ids.get(&target))
            {
                resolver.supers.entry(source).or_default().push(target);
            }
        }
        Ok(resolver)
    }

    fn module(&mut self, file: &str, language: &str) -> Arc<Module> {
        self.module_clock += 1;
        if let Some((age, module)) = self.modules.get_mut(file) {
            *age = self.module_clock;
            return Arc::clone(module);
        }
        // Sources are capped at 1 MiB. Evict the least recently read file.
        if self.modules.len() >= 64 {
            if let Some(oldest) = self
                .modules
                .iter()
                .min_by_key(|(_, (age, _))| *age)
                .map(|(name, _)| name.clone())
            {
                self.modules.remove(&oldest);
            }
        }
        let source = config::relative(self.root, &self.root.join(file))
            .and_then(|path| scan::read_text(&self.root.join(path)))
            .unwrap_or_default();
        let module = Arc::new(Module::new(source, language));
        self.modules
            .insert(file.to_owned(), (self.module_clock, Arc::clone(&module)));
        module
    }

    fn named(&self, name: &str) -> &[usize] {
        self.names.get(name).map(Vec::as_slice).unwrap_or_default()
    }

    fn qualified(&self, name: &str) -> &[usize] {
        self.qualified
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    fn in_file(&self, file: &str) -> &[usize] {
        self.files.get(file).map(Vec::as_slice).unwrap_or_default()
    }

    fn file_node(&self, file: &str) -> Option<usize> {
        self.in_file(file)
            .iter()
            .copied()
            .find(|&id| self.symbols[id].kind == "file")
    }

    fn origin(&self, reference: &Reference) -> Option<&Symbol> {
        self.ids.get(&reference.source).map(|&id| &self.symbols[id])
    }

    fn persist(
        &mut self,
        db: &Connection,
        reference: &Reference,
        hit: Match,
    ) -> rusqlite::Result<()> {
        let target = &self.symbols[hit.target];
        let kind = match reference.kind.as_str() {
            "function_ref" => "references",
            "extends"
                if matches!(target.kind.as_str(), "interface" | "protocol")
                    && self
                        .origin(reference)
                        .is_some_and(|n| !matches!(n.kind.as_str(), "interface" | "protocol")) =>
            {
                "implements"
            }
            "calls" if matches!(target.kind.as_str(), "class" | "struct" | "union") => {
                "instantiates"
            }
            kind => kind,
        };
        let mut metadata = json!({
            "confidence": hit.confidence, "resolvedBy": hit.by, "refName": reference.name,
        });
        if kind != reference.kind {
            metadata["refKind"] = json!(reference.kind);
        }
        if reference.kind == "function_ref" {
            metadata["fnRef"] = json!(true);
        }
        if let Some(framework) = hit.framework {
            metadata["framework"] = json!(framework);
        }
        if let Some(candidates) = &reference.candidates {
            metadata["refCandidates"] = json!(candidates);
        }
        db.prepare_cached(
            "INSERT OR IGNORE INTO edges (source, target, kind, line, col, metadata)
             VALUES (?, ?, ?, ?, ?, ?)",
        )?
        .execute(params![
            reference.source,
            target.id,
            kind,
            reference.line,
            reference.column,
            metadata.to_string()
        ])?;
        db.prepare_cached("DELETE FROM unresolved_refs WHERE id = ?")?
            .execute([reference.row])?;
        if inheritance(kind) {
            if let Some(&source) = self.ids.get(&reference.source) {
                self.supers.entry(source).or_default().push(hit.target);
            }
        }
        Ok(())
    }
}

pub(crate) fn resolve(db: &Connection, root: &Path, changes: &Changes) -> rusqlite::Result<()> {
    changes.retry(db)?;
    if !changes.changed
        && !db.query_row(
            "SELECT EXISTS (SELECT 1 FROM unresolved_refs WHERE status = 'pending')",
            [],
            |row| row.get::<_, bool>(0),
        )?
    {
        return Ok(());
    }
    let mut resolver = Resolver::load(db, root)?;
    let mut statement = db.prepare_cached(
        "SELECT id, from_node_id, reference_name, reference_kind, line, col, file_path, language, candidates
         FROM unresolved_refs
         WHERE status = 'pending' AND id > ?1
           AND (reference_kind IN ('imports', 'extends', 'implements')) = ?2
         ORDER BY id LIMIT ?3",
    )?;
    let mut failed = db.prepare_cached(
        "UPDATE unresolved_refs SET status = 'failed', name_tail = ? WHERE id = ?",
    )?;
    let mut revived = db.prepare_cached(
        "UPDATE unresolved_refs SET status = 'pending'
         WHERE file_path = ? AND status = 'failed'
           AND reference_kind NOT IN ('imports', 'extends', 'implements')",
    )?;
    let mut wired_files = HashSet::new();
    // Wire imports and supertypes before resolving dependent calls.
    for prerequisite in [true, false] {
        let mut after = 0;
        loop {
            let references = statement
                .query_map(params![after, prerequisite, BATCH], |row| {
                    Ok(Reference {
                        row: row.get(0)?,
                        source: row.get(1)?,
                        name: row.get(2)?,
                        kind: row.get(3)?,
                        line: row.get(4)?,
                        column: row.get(5)?,
                        file: row.get(6)?,
                        language: row.get(7)?,
                        candidates: row.get(8)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let Some(last) = references.last() else { break };
            after = last.row;
            for reference in references {
                if let Some(hit) = resolver.resolve_reference(&reference) {
                    resolver.persist(db, &reference, hit)?;
                    if prerequisite && changes.changed && wired_files.insert(reference.file.clone())
                    {
                        revived.execute([&reference.file])?;
                    }
                } else {
                    failed.execute(params![tail(&reference.name), reference.row])?;
                }
            }
        }
    }
    synthesis::refresh(db, &mut resolver)
}

fn tail(name: &str) -> &str {
    name.rsplit(['.', ':']).next().unwrap_or(name)
}

fn js(language: &str) -> bool {
    matches!(language, "javascript" | "jsx" | "typescript" | "tsx")
}

fn family(a: &str, b: &str) -> bool {
    a == b || (js(a) && js(b))
}

fn inheritance(kind: &str) -> bool {
    matches!(kind, "extends" | "implements")
}

fn dirname(file: &str) -> &str {
    file.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn member(kind: &str) -> bool {
    matches!(
        kind,
        "method" | "property" | "field" | "enum_member" | "constructor"
    )
}

fn type_kind(kind: &str) -> bool {
    matches!(
        kind,
        "class" | "struct" | "union" | "interface" | "trait" | "protocol" | "enum" | "type_alias"
    )
}
