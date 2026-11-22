//! Adapted from colbymchenry/codegraph v1.6.2 (6560052), src/db/queries.ts and
//! src/search/identifier-segments.ts. License: MIT (Copyright (c) 2026 Colby Mchenry).

use crate::{
    buffers::VISIBILITIES,
    scan::{FilePath, Source},
};
use rusqlite::{params, Connection};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    sync::LazyLock,
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) fn timestamp(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_millis() as i64,
        Err(error) => -(error.duration().as_millis() as i64),
    }
}

pub(crate) fn file_hashes(db: &Connection) -> rusqlite::Result<HashMap<String, (String, String)>> {
    let mut stmt = db.prepare("SELECT path, content_hash, language FROM files")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, (row.get(1)?, row.get(2)?))))?;
    rows.collect()
}

pub(crate) fn delete_file(db: &Connection, path: &str) -> rusqlite::Result<()> {
    db.prepare_cached("DELETE FROM nodes WHERE file_path = ?")?
        .execute([path])?;
    db.prepare_cached("DELETE FROM files WHERE path = ?")?
        .execute([path])?;
    Ok(())
}

fn json_list(joined: &Option<String>) -> Option<String> {
    joined
        .as_deref()
        .map(|items| json!(items.split('\0').collect::<Vec<_>>()).to_string())
}

pub(crate) fn insert_file(
    db: &Connection,
    file: &FilePath,
    source: &Source,
    now: i64,
) -> rusqlite::Result<()> {
    let errors = source.issue.as_ref().map(|issue| {
        json!([{ "message": issue.message, "filePath": issue.path, "code": issue.code, "severity": "warning" }]).to_string()
    });
    let graph = &source.graph;
    db.prepare_cached(
        "INSERT INTO files (path, content_hash, language, size, modified_at, indexed_at,
                            node_count, errors, generated) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?
    .execute(params![
        file.path,
        source.hash,
        file.language,
        source.size,
        source.modified_at,
        now,
        graph.nodes.len(),
        errors,
        source.generated
    ])?;

    let mut insert_node = db.prepare_cached(
        "INSERT INTO nodes (id, kind, name, qualified_name, file_path, language, start_line, end_line,
                            start_column, end_column, docstring, signature, visibility, is_exported,
                            is_async, is_static, is_abstract, decorators, type_parameters, return_type,
                            updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?;
    let mut insert_segment = db
        .prepare_cached("INSERT OR IGNORE INTO name_segment_vocab (segment, name) VALUES (?, ?)")?;
    let mut seen_names = HashSet::new();
    for node in &graph.nodes {
        let [exported, asynchronous, static_member, abstract_member] = node.flags.0;
        insert_node.execute(params![
            node.id,
            node.kind,
            node.name,
            node.qualified_name,
            file.path,
            file.language,
            node.start_line,
            node.end_line,
            node.start_column,
            node.end_column,
            node.docstring,
            node.signature,
            VISIBILITIES[usize::from(node.visibility)],
            exported,
            asynchronous,
            static_member,
            abstract_member,
            json_list(&node.decorators),
            json_list(&node.type_parameters),
            node.return_type,
            now,
        ])?;
        if node.kind != "file" && seen_names.insert(&node.name) {
            for segment in segments(&node.name) {
                insert_segment.execute(params![segment, node.name])?;
            }
        }
    }

    let mut insert_edge = db.prepare_cached(
        "INSERT OR IGNORE INTO edges (source, target, kind, line, col, metadata)
         VALUES (?, ?, ?, ?, ?, ?)",
    )?;
    for edge in &graph.edges {
        insert_edge.execute(params![
            graph.nodes[edge.source_idx as usize].id,
            graph.nodes[edge.target_idx as usize].id,
            edge.kind,
            edge.line,
            edge.column,
            edge.metadata_json,
        ])?;
    }

    let mut insert_ref = db.prepare_cached(
        "INSERT INTO unresolved_refs (from_node_id, reference_name, reference_kind, line, col,
                                      candidates, file_path, language) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )?;
    for reference in &graph.refs {
        insert_ref.execute(params![
            graph.nodes[reference.from_idx as usize].id,
            reference.reference_name,
            reference.kind,
            reference.line,
            reference.column,
            json_list(&reference.candidates),
            file.path,
            file.language,
        ])?;
    }
    Ok(())
}

fn segments(name: &str) -> HashSet<String> {
    static WORD: LazyLock<regex::Regex> = LazyLock::new(|| crate::literal_regex(r"[\p{L}\p{N}]+"));
    let mut segments = HashSet::new();
    for run in WORD.find_iter(name).map(|run| run.as_str()) {
        let mut start = 0;
        let mut previous = None::<char>;
        let mut chars = run.char_indices().peekable();
        while let Some((index, current)) = chars.next() {
            let boundary = current.is_uppercase()
                && previous.is_some_and(|last| {
                    last.is_lowercase()
                        || last.is_numeric()
                        || (last.is_uppercase()
                            && chars.peek().is_some_and(|(_, next)| next.is_lowercase()))
                });
            if boundary {
                insert_segment(&mut segments, &run[start..index]);
                start = index;
            }
            previous = Some(current);
        }
        insert_segment(&mut segments, &run[start..]);
        if segments.len() == 12 {
            break;
        }
    }
    segments
}

fn insert_segment(segments: &mut HashSet<String>, part: &str) {
    if segments.len() < 12 && !part.chars().all(char::is_numeric) {
        let lower = part.to_lowercase();
        if (2..=32).contains(&lower.encode_utf16().count()) {
            segments.insert(lower);
        }
    }
}
