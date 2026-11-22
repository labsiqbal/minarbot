//! Adapted from colbymchenry/codegraph v1.6.2 (6560052), src/db/queries.ts
//! and src/graph/traversal.ts. License: MIT (Copyright (c) 2026 Colby Mchenry).

mod explore;
mod scoring;
mod tools;

pub use explore::ExploreSession;
pub use tools::mcp_tools;

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashSet, VecDeque},
    path::{Component, Path},
};

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("{0}")]
    Invalid(&'static str),
}

type Result<T> = std::result::Result<T, QueryError>;

#[derive(Clone)]
struct Node {
    id: String,
    kind: String,
    name: String,
    qualified: String,
    file: String,
    start: usize,
    end: usize,
    signature: Option<String>,
    generated: bool,
}

impl Node {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            kind: row.get(1)?,
            name: row.get(2)?,
            qualified: row.get(3)?,
            file: row.get(4)?,
            start: row.get(6)?,
            end: row.get(7)?,
            signature: row.get(8)?,
            generated: row.get(9)?,
        })
    }

    fn location(&self) -> String {
        format!("{}:{}", self.file, self.start)
    }

    fn definition(&self) -> bool {
        !matches!(
            self.kind.as_str(),
            "import" | "export" | "file" | "parameter"
        )
    }
}

const NODE: &str = "n.id,n.kind,n.name,n.qualified_name,n.file_path,n.language,n.start_line,
    n.end_line,n.signature,EXISTS(SELECT 1 FROM files WHERE path=n.file_path AND generated=1)";
const TYPES: &[&str] = &[
    "class",
    "interface",
    "struct",
    "union",
    "trait",
    "protocol",
    "enum",
    "type_alias",
];
const CALLABLE: &[&str] = &["function", "method", "component", "constructor"];
const CONTEXT: &[&str] = &[
    "function",
    "method",
    "class",
    "interface",
    "type_alias",
    "struct",
    "union",
    "trait",
    "component",
    "route",
    "variable",
    "constant",
    "enum",
    "module",
    "namespace",
    "constructor",
];

#[derive(Clone)]
struct Hit {
    node: Node,
    score: f64,
}

fn rank(hits: &mut [Hit]) {
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
}

#[derive(Clone, Copy)]
enum Direction {
    In,
    Out,
}

const CALLS: &[&str] = &["calls", "instantiates"];
const IMPACT: &[&str] = &[
    "calls",
    "imports",
    "extends",
    "implements",
    "references",
    "instantiates",
    "overrides",
];
const BOTH: &[Direction] = &[Direction::Out, Direction::In];

struct Neighbor {
    node: Node,
    line: Option<usize>,
}

pub struct QueryIndex<'a> {
    root: &'a Path,
    db: &'a Connection,
}

impl<'a> QueryIndex<'a> {
    pub(crate) fn new(root: &'a Path, db: &'a Connection) -> Self {
        Self { root, db }
    }

    fn exact(&self, name: &str, kinds: &[&str], limit: usize) -> Result<Vec<Node>> {
        let sql = format!(
            "SELECT {NODE} FROM nodes n WHERE lower(n.name)=lower(?1)
            AND (?2='[]' OR n.kind IN (SELECT value FROM json_each(?2))) LIMIT ?3"
        );
        let mut stmt = self.db.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![name, json!(kinds).to_string(), limit], Node::read)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn search(&self, query: &str, kinds: &[&str], limit: usize) -> Result<Vec<Hit>> {
        let mut text = Vec::new();
        let mut kinds = kinds.to_vec();
        let mut languages = Vec::new();
        let mut paths = Vec::new();
        let mut names = Vec::new();
        for word in query.split_whitespace().take(32) {
            let (field, value) = word.split_once(':').unwrap_or(("", ""));
            match (field, value) {
                ("kind", value) if !value.is_empty() => {
                    kinds.push(if value == "type" { "type_alias" } else { value })
                }
                ("lang", value) if !value.is_empty() => languages.push(value),
                ("path", value) if !value.is_empty() => paths.push(value.to_lowercase()),
                ("name", value) if !value.is_empty() => names.push(value.to_lowercase()),
                _ => text.push(word),
            }
        }
        let text = text.join(" ");
        let fts = text
            .replace("::", " ")
            .replace(['\'', '"', '*', '(', ')', ':', '^'], "");
        let fts = fts
            .split_whitespace()
            .filter(|word| {
                !matches!(
                    word.to_ascii_uppercase().as_str(),
                    "AND" | "OR" | "NOT" | "NEAR"
                )
            })
            .map(|word| format!("\"{word}\"*"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let kinds_json = json!(kinds).to_string();
        let languages = json!(languages).to_string();
        let filters = "(?2='[]' OR n.kind IN (SELECT value FROM json_each(?2)))
            AND (?3='[]' OR n.language IN (SELECT value FROM json_each(?3)))";
        let mut items = Vec::new();
        if !fts.is_empty() {
            let sql = format!(
                "SELECT {NODE}, -bm25(nodes_fts,0,20,5,1,2) AS score
                FROM nodes_fts JOIN nodes n ON n.rowid=nodes_fts.rowid
                WHERE nodes_fts MATCH ?1 AND {filters} ORDER BY score DESC LIMIT ?4"
            );
            let mut stmt = self.db.prepare_cached(&sql)?;
            items = stmt
                .query_map(
                    params![fts, kinds_json, languages, (limit * 5).max(100)],
                    |r| {
                        Ok(Hit {
                            node: Node::read(r)?,
                            score: r.get(10)?,
                        })
                    },
                )?
                .collect::<rusqlite::Result<_>>()?;
        }
        if items.is_empty() && (text.is_empty() || text.len() >= 2) {
            let sql = format!(
                "SELECT {NODE},
                CASE WHEN n.name=?1 THEN 1.0 WHEN instr(lower(n.name),lower(?1))=1 THEN 0.9
                WHEN instr(lower(n.name),lower(?1))>0 THEN 0.8 ELSE 0.7 END AS score
                FROM nodes n WHERE (instr(lower(n.name),lower(?1))>0 OR
                instr(lower(n.qualified_name),lower(?1))>0) AND {filters}
                ORDER BY score DESC,length(n.name),n.name LIMIT ?4"
            );
            let mut stmt = self.db.prepare_cached(&sql)?;
            items = stmt
                .query_map(params![text, kinds_json, languages, limit * 5], |r| {
                    Ok(Hit {
                        node: Node::read(r)?,
                        score: r.get(10)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
        }
        let highest = items.iter().map(|hit| hit.score).fold(0.0, f64::max);
        let mut seen: HashSet<_> = items.iter().map(|hit| hit.node.id.clone()).collect();
        let sql = format!(
            "SELECT {NODE} FROM nodes n WHERE lower(n.name)=lower(?1)
            AND {filters} LIMIT 20"
        );
        let mut stmt = self.db.prepare_cached(&sql)?;
        for term in text.split_whitespace().filter(|term| term.len() >= 2) {
            for node in stmt.query_map(params![term, kinds_json, languages], Node::read)? {
                let node = node?;
                if seen.insert(node.id.clone()) {
                    items.push(Hit {
                        node,
                        score: highest,
                    });
                }
            }
        }
        let path_rank = scoring::PathRank::new(&text);
        for hit in &mut items {
            hit.score += scoring::kind(&hit.node.kind)
                + path_rank.score(&hit.node.file)
                + scoring::name(&hit.node.name, &text);
            if hit.node.generated {
                hit.score -= 15.0;
            }
        }
        rank(&mut items);
        items.retain(|hit| {
            (paths.is_empty()
                || paths
                    .iter()
                    .any(|path| hit.node.file.to_lowercase().contains(path)))
                && (names.is_empty()
                    || names
                        .iter()
                        .any(|name| hit.node.name.to_lowercase().contains(name)))
        });
        items.truncate(limit);
        Ok(items)
    }

    fn file_nodes(&self, path: &str, limit: usize) -> Result<Vec<Node>> {
        let sql = format!(
            "SELECT {NODE} FROM nodes n WHERE n.file_path=?
            ORDER BY n.start_line,n.start_column LIMIT ?"
        );
        let mut stmt = self.db.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![path, limit], Node::read)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn file_matches(&self, hint: &str) -> Result<Vec<String>> {
        let path = hint.replace('\\', "/");
        let path = path.trim_start_matches("./").trim_start_matches('/');
        let mut stmt = self.db.prepare_cached(
            "SELECT path FROM files WHERE path=?1 OR substr(path,-length(?1)-1)='/'||?1
             ORDER BY path<>?1,path LIMIT 25",
        )?;
        let mut found = stmt
            .query_map([path], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if found.first().is_some_and(|first| first == path) {
            found.truncate(1);
        }
        Ok(found)
    }

    fn neighbors(
        &self,
        id: &str,
        direction: Direction,
        kinds: &[&str],
        limit: usize,
    ) -> Result<Vec<Neighbor>> {
        let (from, to) = match direction {
            Direction::In => ("target", "source"),
            Direction::Out => ("source", "target"),
        };
        let sql = format!("SELECT {NODE}, min(e.line) FROM edges e JOIN nodes n ON n.id=e.{to}
            WHERE e.{from}=?1 AND (?2='[]' OR e.kind IN (SELECT value FROM json_each(?2)))
            GROUP BY n.id ORDER BY min(CASE e.kind WHEN 'contains' THEN 0 WHEN 'calls' THEN 1 ELSE 2 END),min(e.id) LIMIT ?3");
        let mut stmt = self.db.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![id, json!(kinds).to_string(), limit], |row| {
            Ok(Neighbor {
                node: Node::read(row)?,
                line: row.get(10)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn traverse(
        &self,
        root: &Node,
        depth: usize,
        limit: usize,
        direction: &[Direction],
        edges: &[&str],
        kinds: &[&str],
    ) -> Result<Vec<(Node, usize)>> {
        let mut queue = VecDeque::from([(root.clone(), 0)]);
        let mut seen = HashSet::from([root.id.clone()]);
        let mut nodes = Vec::new();
        while let Some((node, level)) = queue.pop_front() {
            if level < depth && seen.len() < limit {
                for &direction in direction {
                    for next in self.neighbors(&node.id, direction, edges, limit * 4)? {
                        if seen.len() >= limit {
                            break;
                        }
                        let next = next.node;
                        if (kinds.is_empty() || kinds.contains(&next.kind.as_str()))
                            && seen.insert(next.id.clone())
                        {
                            queue.push_back((next, level + 1));
                        }
                    }
                }
            }
            nodes.push((node, level));
        }
        Ok(nodes)
    }

    fn dependents(&self, file: &str) -> Result<(usize, Vec<String>)> {
        let mut stmt = self.db.prepare_cached(
            "SELECT source.file_path, count(*) OVER () FROM nodes target
             JOIN edges e ON e.target=target.id JOIN nodes source ON source.id=e.source
             WHERE target.file_path=?1 AND source.file_path<>?1 GROUP BY source.file_path
             ORDER BY count(*) DESC,source.file_path LIMIT 8",
        )?;
        let mut rows = stmt.query([file])?;
        let mut files = Vec::new();
        let mut count = 0;
        while let Some(row) = rows.next()? {
            files.push(row.get(0)?);
            count = row.get(1)?;
        }
        Ok((count, files))
    }

    fn source(&self, file: &str) -> Result<Option<(String, bool)>> {
        let path = Path::new(file);
        if !path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        {
            return Ok(None);
        }
        let record = self
            .db
            .query_row(
                "SELECT content_hash,language FROM files WHERE path=?",
                [file],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((hash, language)) = record else {
            return Ok(None);
        };
        if matches!(
            language.as_str(),
            "yaml" | "properties" | "json" | "toml" | "env" | "ini"
        ) {
            return Ok(None);
        }
        Ok(crate::scan::read_text(&self.root.join(path)).map(|text| {
            let current = format!("{:x}", Sha256::digest(text.as_bytes()));
            (text, current == hash)
        }))
    }

    pub fn stats(&self) -> Result<Value> {
        Ok(self.db.query_row(
            "SELECT count(*),coalesce(sum(errors IS NOT NULL),0),
             coalesce((SELECT updated_at FROM project_metadata WHERE key='minarbot_last_sync'),max(indexed_at),0),
             (SELECT count(*) FROM nodes),(SELECT count(*) FROM edges),
             (SELECT value FROM project_metadata WHERE key='minarbot_last_sync')
             FROM files", [], |row| Ok(json!({
                "files":row.get::<_,usize>(0)?, "errors":row.get::<_,usize>(1)?,
                "indexedAt":row.get::<_,i64>(2)?, "nodes":row.get::<_,usize>(3)?,
                "edges":row.get::<_,usize>(4)?,
                "changes":row.get::<_,Option<String>>(5)?.and_then(|value| serde_json::from_str::<Value>(&value).ok())
            })))?)
    }

    /// Run one read against a single graph snapshot while another turn may sync.
    pub fn call(&self, session: &mut ExploreSession, name: &str, args: &Value) -> Result<Value> {
        let snapshot = self.db.unchecked_transaction()?;
        let mut served = session.clone();
        let text = QueryIndex::new(self.root, &snapshot).run(&mut served, name, args)?;
        snapshot.commit()?;
        *session = served;
        Ok(json!({"content":[{"type":"text","text":text}]}))
    }
}
