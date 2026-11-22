//! Adapted from colbymchenry/codegraph v1.6.2 (6560052), src/mcp/tools.ts.
//! License: MIT (Copyright (c) 2026 Colby Mchenry).

use super::*;
use std::{collections::BTreeMap, fmt::Write as _, sync::LazyLock};

pub fn mcp_tools() -> &'static Value {
    static TOOLS: LazyLock<Value> = LazyLock::new(|| {
        let spec = |name, description, properties, required: &[&str]| {
            json!({
                "name":name, "description":description,
                "inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},
                "annotations":{"readOnlyHint":true,"openWorldHint":false}
            })
        };
        let relation = || json!({"symbol":{"type":"string"},"file":{"type":"string"},"limit":{"type":"integer"}});
        let mut explore = spec("codegraph_explore",
            "Explore code before reading or editing. Name symbols, paths, or a short question. Returns numbered source, call paths, and blast radius.",
            json!({"query":{"type":"string"},"maxFiles":{"type":"integer"}}), &["query"]);
        explore["_meta"] = json!({"anthropic/alwaysLoad":true});
        json!([
            explore,
            spec("codegraph_search","Find symbol locations by name or prefix.",
                json!({"query":{"type":"string"},"kind":{"type":"string"},"limit":{"type":"integer"}}), &["query"]),
            spec("codegraph_callers","List direct callers. Use file to narrow overloads.",relation(), &["symbol"]),
            spec("codegraph_callees","List direct callees. Use file to narrow overloads.",relation(), &["symbol"]),
            spec("codegraph_impact","Find transitive dependents before changing a symbol.",
                json!({"symbol":{"type":"string"},"file":{"type":"string"},"depth":{"type":"integer"}}), &["symbol"]),
            spec("codegraph_node","Read an indexed file (file, offset, limit, or symbolsOnly) or symbol (symbol, optional file and line, includeCode for source).",
                json!({"symbol":{"type":"string"},"file":{"type":"string"},"line":{"type":"integer"},
                    "includeCode":{"type":"boolean"},"offset":{"type":"integer"},"limit":{"type":"integer"},
                    "symbolsOnly":{"type":"boolean"}}), &[]),
            spec("codegraph_status","Show project index health.",json!({}), &[]),
            spec("codegraph_files","List indexed files by path or glob. Formats: tree, flat, grouped.",
                json!({"path":{"type":"string"},"pattern":{"type":"string"},
                    "format":{"type":"string","enum":["tree","flat","grouped"]},
                    "includeMetadata":{"type":"boolean"},"maxDepth":{"type":"integer"}}), &[]),
        ])
    });
    &TOOLS
}

pub(super) fn input<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args[key]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(super) fn number(args: &Value, key: &str, default: i64, max: i64) -> usize {
    args[key].as_i64().unwrap_or(default).clamp(1, max) as usize
}

pub(super) fn numbered(lines: &[&str], start: usize, end: usize, budget: usize) -> (String, usize) {
    let mut body = String::new();
    let mut last = start.saturating_sub(1);
    for (index, line) in lines.iter().enumerate().take(end).skip(last) {
        let number = index + 1;
        if body.len() + number.to_string().len() + line.len() + 2 > budget {
            break;
        }
        let _ = writeln!(body, "{number}\t{line}");
        last = number;
    }
    (body, last)
}

fn capped(mut text: String, limit: usize) -> String {
    if text.len() > limit {
        let end = text.floor_char_boundary(limit);
        text.truncate(text[..end].rfind('\n').filter(|&i| i > 0).unwrap_or(end));
        text.push_str("\n… more results omitted");
    }
    text
}

impl QueryIndex<'_> {
    pub(super) fn run(
        &self,
        session: &mut ExploreSession,
        name: &str,
        args: &Value,
    ) -> Result<String> {
        let text = match name {
            "codegraph_search" => self.search_tool(args)?,
            "codegraph_callers" => self.graph_tool(args, Some(Direction::In))?,
            "codegraph_callees" => self.graph_tool(args, Some(Direction::Out))?,
            "codegraph_impact" => self.graph_tool(args, None)?,
            "codegraph_node" => self.node_tool(args)?,
            "codegraph_explore" => return self.explore_tool(session, args),
            "codegraph_status" => self.status_tool()?,
            "codegraph_files" => self.files_tool(args)?,
            _ => return Err(QueryError::Invalid("Unknown CodeGraph tool.")),
        };
        Ok(capped(text, 38_000))
    }

    fn search_tool(&self, args: &Value) -> Result<String> {
        let query = input(args, "query").ok_or(QueryError::Invalid("Provide a query."))?;
        let kind = input(args, "kind").map(|k| if k == "type" { "type_alias" } else { k });
        let kinds = kind.as_slice();
        let mut hits = self.search(query, kinds, number(args, "limit", 10, 100))?;
        if hits.is_empty() {
            return Ok(format!("No results found for \"{query}\"."));
        }
        hits.sort_by_key(|hit| hit.node.generated);
        let mut text = format!("**Search results for {query}**\n");
        for Hit { node, .. } in hits {
            let _ = writeln!(
                text,
                "- `{}` ({}) - {}",
                node.name,
                node.kind,
                node.location()
            );
        }
        Ok(text)
    }

    pub(super) fn definitions(&self, symbol: &str, file: Option<&str>) -> Result<Vec<Node>> {
        let normalized = symbol.replace('.', "::");
        let tail = normalized.rsplit("::").next().unwrap_or(&normalized);
        let mut found = self.exact(tail, &[], 1000)?;
        found.retain(|node| {
            node.definition()
                && (tail == normalized
                    || node.qualified == normalized
                    || node
                        .qualified
                        .strip_suffix(&normalized)
                        .is_some_and(|prefix| prefix.ends_with("::")))
                && file.is_none_or(|file| {
                    node.file == file
                        || node
                            .file
                            .strip_suffix(file)
                            .is_some_and(|prefix| prefix.ends_with('/'))
                })
        });
        if found.is_empty() && tail == normalized {
            found = self
                .search(symbol, CONTEXT, 15)?
                .into_iter()
                .filter(|hit| {
                    hit.node.name.eq_ignore_ascii_case(symbol)
                        && file.is_none_or(|file| hit.node.file.ends_with(file))
                })
                .map(|hit| hit.node)
                .collect();
        }
        found.sort_by(|a, b| a.file.cmp(&b.file).then(a.start.cmp(&b.start)));
        Ok(found)
    }

    fn graph_tool(&self, args: &Value, direction: Option<Direction>) -> Result<String> {
        let symbol = input(args, "symbol").ok_or(QueryError::Invalid("Provide a symbol."))?;
        let nodes = self.definitions(symbol, input(args, "file"))?;
        if nodes.is_empty() {
            return Ok(format!("Symbol \"{symbol}\" not found in the index."));
        }
        let label = match direction {
            Some(Direction::In) => "Callers",
            Some(Direction::Out) => "Callees",
            None => "Impact",
        };
        let mut groups = BTreeMap::<(_, _), Vec<_>>::new();
        for node in nodes {
            groups
                .entry((node.file.clone(), node.qualified.clone()))
                .or_default()
                .push(node);
        }
        let limit = number(args, "limit", 20, 100);
        let depth = number(args, "depth", 2, 10);
        let mut text = format!("**{label} of {symbol}**\n");
        for group in groups.values().take(25) {
            let mut seen = HashSet::new();
            let node = &group[0];
            let _ = writeln!(text, "\n**{}** - {}", node.qualified, node.location());
            for root in group {
                let neighbors = match direction {
                    Some(direction) => self
                        .neighbors(&root.id, direction, CALLS, limit + 1)?
                        .into_iter()
                        .map(|next| (next.node, next.line))
                        .collect(),
                    None => self
                        .traverse(root, depth, 200, &[Direction::In], IMPACT, &[])?
                        .into_iter()
                        .skip(1)
                        .map(|(node, _)| (node, None))
                        .collect::<Vec<_>>(),
                };
                for (node, call_line) in neighbors {
                    if node.id != root.id && seen.insert(node.id.clone()) && seen.len() <= limit {
                        let _ = write!(
                            text,
                            "- `{}` ({}) - {}",
                            node.name,
                            node.kind,
                            node.location()
                        );
                        if let Some(line) = call_line {
                            let _ = write!(text, " (call at line {line})");
                        }
                        text.push('\n');
                    }
                }
            }
            if seen.is_empty() {
                text.push_str("- None found\n");
            } else if seen.len() > limit {
                text.push_str("- … more results\n");
            }
        }
        if groups.len() > 25 {
            text.push_str("\nMore definitions match. Narrow with file.");
        }
        Ok(text)
    }

    fn node_tool(&self, args: &Value) -> Result<String> {
        let file = input(args, "file");
        let Some(symbol) = input(args, "symbol") else {
            return self.file_view(
                file.ok_or(QueryError::Invalid("Provide a file or symbol."))?,
                args,
            );
        };
        let mut nodes = self.definitions(symbol, file)?;
        if let Some(line) = args["line"].as_u64() {
            let line = usize::try_from(line).unwrap_or(usize::MAX);
            nodes.sort_by_key(|node| {
                (
                    line < node.start || line > node.end,
                    line.abs_diff(node.start),
                )
            });
            nodes.truncate(1);
        }
        if nodes.is_empty() {
            return Ok(format!("Symbol \"{symbol}\" not found in the index."));
        }
        let mut text = format!("**{} definitions for {symbol}**\n", nodes.len());
        for node in nodes.iter().take(25) {
            let _ = writeln!(
                text,
                "\n**{}** ({}) - {}",
                node.qualified,
                node.kind,
                node.location()
            );
            if let Some(signature) = &node.signature {
                let _ = writeln!(text, "{signature}");
            }
            if args["includeCode"] == true && text.len() < 24_000 {
                match self.source(&node.file)? {
                    Some((source, true)) => {
                        let (body, end) = numbered(&source.split('\n').collect::<Vec<_>>(),
                            node.start,node.end, 6000);
                        text.push_str(&body);
                        if end < node.end {
                            let _ = writeln!(text, "… source continues at {}:{}", node.file, end + 1);
                        }
                    }
                    _ => text.push_str("Source unavailable or changed since indexing. Use file mode for current lines.\n"),
                }
            }
            for (title, direction) in [("Calls", Direction::Out), ("Called by", Direction::In)] {
                let links = self.neighbors(&node.id, direction, CALLS, 8)?;
                if !links.is_empty() {
                    let links: Vec<_> = links
                        .iter()
                        .map(|next| format!("{} ({})", next.node.name, next.node.location()))
                        .collect();
                    let _ = writeln!(text, "{title}: {}", links.join(", "));
                }
            }
        }
        if nodes.len() > 25 {
            text.push_str("\nMore definitions match. Narrow with file and line.");
        }
        Ok(text)
    }

    fn file_view(&self, file: &str, args: &Value) -> Result<String> {
        let mut paths = self.file_matches(file)?;
        let (mut start, mut end) = (1, usize::MAX);
        if paths.is_empty() {
            let (hint, from, to) = super::explore::file_anchor(file);
            paths = self.file_matches(hint)?;
            start = from.unwrap_or(start);
            end = to.unwrap_or(end);
        }
        let path = match paths.as_slice() {
            [path] => path,
            [] => return Ok(format!("No indexed file matches \"{file}\".")),
            _ => {
                return Ok(format!(
                    "Several indexed files match \"{file}\". Pass a longer path.\n{}",
                    paths.join("\n")
                ))
            }
        };
        let (count, dependents) = self.dependents(path)?;
        let deps = if count == 0 {
            "no other indexed file depends on it".to_owned()
        } else {
            format!("used by {count} indexed files: {}", dependents.join(", "))
        };
        let mut text = format!("**{path}** - {deps}\n\n");
        if args["symbolsOnly"] == true {
            for node in self
                .file_nodes(path, 350)?
                .into_iter()
                .filter(Node::definition)
                .take(200)
            {
                let _ = writeln!(text, "- `{}` ({}) :{}", node.name, node.kind, node.start);
            }
        } else if let Some((source, _)) = self.source(path)? {
            let lines: Vec<_> = source.split('\n').collect();
            start = number(
                args,
                "offset",
                start.min(i64::MAX as usize) as i64,
                i64::MAX,
            );
            let limit = number(
                args,
                "limit",
                end.saturating_sub(start).saturating_add(1).min(2000) as i64,
                2000,
            );
            end = start.saturating_add(limit - 1).min(lines.len());
            if start > lines.len() {
                let _ = writeln!(text, "Offset {start} exceeds {} lines.", lines.len());
            } else {
                let (body, last) =
                    numbered(&lines, start, end, 36_000usize.saturating_sub(text.len()));
                text.push_str(&body);
                if last < lines.len() {
                    let _ = writeln!(text, "… use offset {} to continue.", last + 1);
                }
            }
        } else {
            text.push_str("Source unavailable. Use symbolsOnly for the indexed symbol map.");
        }
        Ok(text)
    }

    fn status_tool(&self) -> Result<String> {
        let stats = self.stats()?;
        let mut text = format!("**Minarbot CodeGraph**\n- Files: {}\n- Nodes: {}\n- Edges: {}\n- Files with errors: {}\n- Last indexed (Unix ms): {}",
            stats["files"],stats["nodes"],stats["edges"],stats["errors"],stats["indexedAt"]);
        if let Some(changes) = stats["changes"].as_object() {
            let _ = write!(
                text,
                "\n- Last sync: {} added, {} modified, {} removed",
                changes["added"], changes["modified"], changes["removed"]
            );
        }
        Ok(text)
    }

    fn files_tool(&self, args: &Value) -> Result<String> {
        let filter = input(args, "path").unwrap_or("").replace('\\', "/");
        let filter = filter.trim_matches('/').trim_start_matches("./");
        let filter = filter.trim_end_matches('/');
        let filter = if filter == "." { "" } else { filter };
        let prefix = format!("{filter}/");
        let full = |pattern: &str| {
            globset::GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()
                .map(|glob| glob.compile_matcher())
        };
        let (glob, basename) = if let Some(pattern) = input(args, "pattern") {
            (
                Some(full(pattern).map_err(|_| QueryError::Invalid("Invalid file glob."))?),
                !pattern.contains('/'),
            )
        } else {
            (None, false)
        };
        let mut stmt = self
            .db
            .prepare_cached("SELECT path,language,node_count FROM files ORDER BY path")?;
        let mut rows = stmt.query([])?;
        let mut files = Vec::new();
        while let Some(row) = rows.next()? {
            let path: String = row.get(0)?;
            let relative = if filter.is_empty() || path == filter {
                Some(path.as_str())
            } else {
                path.strip_prefix(&prefix)
            };
            let Some(relative) = relative else {
                continue;
            };
            if glob.as_ref().is_some_and(|glob| {
                !glob.is_match(if basename {
                    path.rsplit('/').next().unwrap_or(&path)
                } else {
                    relative
                })
            }) {
                continue;
            }
            files.push((path, row.get::<_, String>(1)?, row.get::<_, usize>(2)?));
        }
        if files.is_empty() {
            return Ok("No indexed files match.".into());
        }
        let format = input(args, "format").unwrap_or("tree");
        let max_depth = number(args, "maxDepth", 20, 20);
        if format == "grouped" {
            files.sort_by(|a, b| a.1.cmp(&b.1));
        }
        let mut text = format!("**Files ({})**\n", files.len());
        let mut previous = Vec::new();
        let mut last_language = String::new();
        for (path, language, count) in &files {
            let meta = if args["includeMetadata"] != false {
                format!(" ({language}, {count} symbols)")
            } else {
                String::new()
            };
            if format == "tree" {
                let rel = if filter.is_empty() {
                    path
                } else {
                    path.strip_prefix(&prefix).unwrap_or(path)
                };
                let parts: Vec<_> = rel.split('/').collect();
                for (index, part) in parts.iter().enumerate().take(max_depth) {
                    if index + 1 == parts.len() {
                        let _ = writeln!(text, "{}- {part}{meta}", "  ".repeat(index));
                    } else if previous.get(..=index) != parts.get(..=index) {
                        let _ = writeln!(text, "{}- {part}/", "  ".repeat(index));
                    }
                }
                previous = parts;
            } else {
                if format == "grouped" && &last_language != language {
                    let _ = writeln!(text, "\n**{language}**");
                    last_language.clone_from(language);
                }
                let _ = writeln!(text, "- {path}{meta}");
            }
            if text.len() > 38_000 {
                break;
            }
        }
        Ok(text)
    }
}
