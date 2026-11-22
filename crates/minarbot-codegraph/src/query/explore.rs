//! Adapted from colbymchenry/codegraph v1.6.2 (6560052), src/context/index.ts,
//! src/mcp/tools.ts and src/mcp/explore-dedup.ts.
//! License: MIT (Copyright (c) 2026 Colby Mchenry).

use super::{
    scoring::*,
    tools::{input, number, numbered},
    *,
};
use std::{
    collections::{BTreeMap, HashMap},
    fmt::Write as _,
    sync::LazyLock,
};

const SEARCH_LIMIT: usize = 8;
const NODE_CAP: usize = 200;
const GLUE_CAP: usize = 60;
const HIERARCHY: &[&str] = &["extends", "implements"];
const RANK_EDGES: &[&str] = &[
    "calls",
    "instantiates",
    "references",
    "extends",
    "implements",
    "overrides",
    "navigates",
    "returns",
    "type_of",
    "imports",
];

/// One instance per Minarbot turn's MCP connection. Only lines actually
/// emitted for the same file content become eligible for back-references.
#[derive(Clone, Default)]
pub struct ExploreSession {
    served: HashMap<String, ServedFile>,
}

#[derive(Clone)]
struct ServedFile {
    fingerprint: [u8; 32],
    ranges: Vec<(usize, usize)>,
}

impl ExploreSession {
    fn ranges(&mut self, path: &str, source: &str) -> &mut Vec<(usize, usize)> {
        // Bound retained state for long-running sessions.
        if self.served.len() >= 128 && !self.served.contains_key(path) {
            self.served.clear();
        }
        let fingerprint = Sha256::digest(source.as_bytes()).into();
        let file = self.served.entry(path.to_owned()).or_insert(ServedFile {
            fingerprint,
            ranges: Vec::new(),
        });
        if file.fingerprint != fingerprint {
            file.fingerprint = fingerprint;
            file.ranges.clear();
        }
        &mut file.ranges
    }
}

fn merge_spans(spans: &mut Vec<(usize, usize)>) {
    spans.sort_unstable();
    let mut write = 0;
    for read in 0..spans.len() {
        let (start, end) = spans[read];
        if write > 0 && start <= spans[write - 1].1.saturating_add(1) {
            spans[write - 1].1 = spans[write - 1].1.max(end);
        } else {
            spans[write] = (start, end);
            write += 1;
        }
    }
    spans.truncate(write);
}

pub(super) fn file_anchor(token: &str) -> (&str, Option<usize>, Option<usize>) {
    static ANCHOR: LazyLock<regex::Regex> =
        LazyLock::new(|| crate::literal_regex(r"(?::|#L)([0-9]+)(?:-L?([0-9]+))?$"));
    let Some(captures) = ANCHOR.captures(token) else {
        return (token, None, None);
    };
    let Some(suffix) = captures.get(0) else {
        return (token, None, None);
    };
    let line = |i| {
        captures
            .get(i)
            .and_then(|m| m.as_str().parse::<usize>().ok())
            .filter(|&n| n > 0)
    };
    (&token[..suffix.start()], line(1), line(2))
}

fn merge_hits(out: &mut Vec<Hit>, hits: impl IntoIterator<Item = Hit>) {
    let mut index: HashMap<_, _> = out
        .iter()
        .enumerate()
        .map(|(i, hit)| (hit.node.id.clone(), i))
        .collect();
    for hit in hits {
        match index.entry(hit.node.id.clone()) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                let current = &mut out[*entry.get()];
                current.score = current.score.max(hit.score);
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(out.len());
                out.push(hit);
            }
        }
    }
}

fn term_groups(terms: &[String]) -> Vec<Vec<&str>> {
    let mut sorted: Vec<_> = terms.iter().map(String::as_str).collect();
    sorted.sort_by_key(|term| std::cmp::Reverse(term.len()));
    let mut assigned = HashSet::new();
    let mut groups = Vec::new();
    for &term in &sorted {
        if !assigned.insert(term) {
            continue;
        }
        let mut group = vec![term];
        for &other in &sorted {
            if (term.contains(other) || other.contains(term)) && assigned.insert(other) {
                group.push(other);
            }
        }
        groups.push(group);
    }
    groups
}

#[derive(Default)]
struct FileContext {
    nodes: Vec<(Node, f64)>,
    anchors: Vec<(usize, usize)>,
    pin: Option<usize>,
    named: bool,
    entry: bool,
    weight: f64,
    score: f64,
    terms: usize,
}

impl QueryIndex<'_> {
    fn segment_seeds(&self, query: &str) -> Result<Vec<String>> {
        let words = segment_terms(query);
        let mut variants = BTreeMap::new();
        for word in &words {
            variants.entry(word.as_str()).or_insert(word.as_str());
            if let Some(base) = word
                .strip_suffix('s')
                .filter(|base| base.len() >= 4 && !word.ends_with("ss"))
            {
                variants.entry(base).or_insert(word.as_str());
            }
            if let Some(base) = word.strip_suffix("es").filter(|base| base.len() >= 4) {
                variants.entry(base).or_insert(word.as_str());
            }
        }
        if variants.is_empty() {
            return Ok(Vec::new());
        }
        let values = json!(variants).to_string();
        let mut stmt = self.db.prepare_cached(
            "SELECT v.name FROM name_segment_vocab v JOIN json_each(?) t ON v.segment=t.key
             WHERE EXISTS(SELECT 1 FROM nodes n WHERE n.name=v.name AND n.kind NOT IN ('import','file','export'))
             GROUP BY v.name HAVING count(DISTINCT t.value)>=2
             ORDER BY count(DISTINCT t.value) DESC,length(v.name),v.name LIMIT 24",
        )?;
        let mut names = stmt
            .query_map([values], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if names.is_empty() {
            let mut count = self.db.prepare_cached(
                "SELECT count(*) FROM (
                SELECT 1 FROM name_segment_vocab WHERE segment=? LIMIT 26)",
            )?;
            let mut rare = Vec::new();
            for (segment, word) in variants {
                if word.len() < 5 {
                    continue;
                }
                let n = count.query_row([segment], |row| row.get::<_, usize>(0))?;
                if (2..=25).contains(&n) {
                    rare.push((n, segment));
                }
            }
            let mut stmt = self.db.prepare_cached(
                "SELECT v.name FROM name_segment_vocab v WHERE v.segment=? AND lower(v.name)<>v.segment
                 AND EXISTS(SELECT 1 FROM nodes n WHERE n.name=v.name AND n.kind NOT IN ('import','file','export'))
                 ORDER BY length(v.name),v.name LIMIT 12",
            )?;
            rare.sort_unstable();
            for (_, segment) in rare.into_iter().take(2) {
                names.extend(
                    stmt.query_map([segment], |row| row.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?,
                );
            }
            names.sort_by_key(String::len);
        }
        let mut seen = HashSet::new();
        names.retain(|name| seen.insert(name.clone()));
        names.truncate(SEARCH_LIMIT);
        Ok(names)
    }

    fn exact_hits(&self, names: &[String], limit: usize) -> Result<Vec<Hit>> {
        let mut distinctive_files = HashSet::new();
        let mut by_name = Vec::new();
        let per_name = limit.div_ceil(names.len().max(1)).max(8);
        for name in names {
            let matches = self.exact(name, CONTEXT, (per_name * 3).max(50))?;
            let files: HashSet<_> = matches.iter().map(|node| node.file.as_str()).collect();
            if files.len() < 10 {
                distinctive_files.extend(files.into_iter().map(str::to_owned));
            }
            by_name.push(matches);
        }
        let mut seen = HashSet::new();
        let mut hits = Vec::new();
        for matches in by_name {
            let mut local: Vec<_> = matches
                .into_iter()
                .map(|node| Hit {
                    score: if distinctive_files.contains(&node.file) {
                        21.0
                    } else {
                        1.0
                    },
                    node,
                })
                .collect();
            rank(&mut local);
            hits.extend(
                local
                    .into_iter()
                    .filter(|hit| seen.insert(hit.node.id.clone()))
                    .take(per_name),
            );
        }
        rank(&mut hits);
        hits.truncate(limit);
        Ok(hits)
    }

    fn roots(&self, query: &str) -> Result<Vec<Hit>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let symbols = symbols(query);
        let seeds = self.segment_seeds(query)?;
        let mut exact = self.exact_hits(&symbols, SEARCH_LIMIT * 5)?;
        merge_hits(
            &mut exact,
            self.exact_hits(&seeds, SEARCH_LIMIT * 3)?
                .into_iter()
                .map(|mut hit| {
                    hit.score *= 0.6;
                    hit
                }),
        );
        let mut file_terms = HashMap::<_, HashSet<_>>::new();
        for hit in &exact {
            file_terms
                .entry(hit.node.file.clone())
                .or_default()
                .insert(hit.node.name.to_lowercase());
        }
        for hit in &mut exact {
            hit.score += file_terms[&hit.node.file].len().saturating_sub(1) as f64 * 20.0;
        }
        rank(&mut exact);
        exact.truncate(SEARCH_LIMIT * 2);

        let mut prefixes = Vec::new();
        let mut seen = HashSet::new();
        for term in symbols
            .iter()
            .map(|name| name.to_lowercase())
            .chain(symbols.iter().flat_map(|name| stems(&name.to_lowercase())))
            .filter(|term| seen.insert(term.clone()))
        {
            let mut hits = self.search(&term, TYPES, 30)?;
            hits.retain(|hit| hit.node.name.to_lowercase().starts_with(&term));
            for hit in &mut hits {
                let bonus =
                    (10.0 - (hit.node.name.len().saturating_sub(term.len())) as f64 / 3.0).max(0.0);
                hit.score += 15.0 + bonus;
            }
            rank(&mut hits);
            merge_hits(&mut prefixes, hits.into_iter().take(SEARCH_LIMIT));
        }
        let known: HashSet<_> = exact.iter().map(|hit| hit.node.id.clone()).collect();
        exact.extend(
            prefixes
                .into_iter()
                .filter(|hit| !known.contains(&hit.node.id)),
        );
        rank(&mut exact);
        exact.truncate(SEARCH_LIMIT * 3);

        let terms = terms(query, true);
        let mut text = Vec::new();
        let mut hits_per_node = HashMap::new();
        for term in &terms {
            let results = self.search(term, CONTEXT, SEARCH_LIMIT * 2)?;
            for hit in &results {
                *hits_per_node.entry(hit.node.id.clone()).or_insert(0_usize) += 1;
            }
            merge_hits(&mut text, results);
        }
        for hit in &mut text {
            hit.score += hits_per_node[&hit.node.id].saturating_sub(1) as f64 * 5.0;
        }
        rank(&mut text);
        let exact_ids: HashSet<_> = exact.iter().map(|hit| hit.node.id.clone()).collect();
        let distinctive: HashSet<_> = symbols
            .iter()
            .filter(|word| distinctive(word))
            .map(|word| word.to_lowercase())
            .collect();
        merge_hits(&mut exact, text.into_iter().take(SEARCH_LIMIT * 2));
        let groups = term_groups(&terms);
        for hit in &mut exact {
            let lower = hit.node.name.to_lowercase();
            let path = hit.node.file.to_lowercase();
            if is_test(&path) && !test_query(query) {
                hit.score *= 0.3;
            }
            if groups.len() >= 2 {
                let dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
                let count = groups
                    .iter()
                    .filter(|group| {
                        group.iter().any(|term| {
                            lower.contains(term) || dir.split('/').any(|part| part == *term)
                        })
                    })
                    .count();
                if count >= 2 {
                    hit.score *= 1.0 + count as f64 * 0.5;
                } else if !distinctive.contains(&lower) {
                    hit.score *= if exact_ids.contains(&hit.node.id) {
                        0.3
                    } else {
                        0.6
                    };
                }
            }
        }
        self.add_infix_hits(query, &symbols, &mut exact)?;
        rank(&mut exact);
        exact.retain(|hit| hit.score >= 0.2);
        exact.truncate(SEARCH_LIMIT);
        Ok(exact)
    }

    fn add_infix_hits(&self, query: &str, symbols: &[String], hits: &mut Vec<Hit>) -> Result<()> {
        let sql = format!("SELECT {NODE} FROM nodes n WHERE instr(lower(n.name),?1)>0
            AND n.kind IN (SELECT value FROM json_each(?2)) ORDER BY length(n.name),n.name LIMIT 200");
        let mut stmt = self.db.prepare_cached(&sql)?;
        let type_kinds = json!(TYPES).to_string();
        let callable_kinds = json!(CALLABLE).to_string();
        let path_rank = PathRank::new(query);
        let existing: HashSet<_> = hits.iter().map(|hit| hit.node.id.clone()).collect();
        let mut seen = HashSet::new();
        let mut compound: HashMap<String, (Node, usize)> = HashMap::new();
        let mut camel: HashMap<String, (Hit, usize)> = HashMap::new();
        for term in symbols
            .iter()
            .map(|name| name.to_lowercase())
            .filter(|word| word.len() >= 3 && seen.insert(word.clone()))
        {
            let mut candidates = Vec::new();
            for kinds in [&type_kinds, &callable_kinds] {
                for row in stmt.query_map(params![term, kinds], Node::read)? {
                    let node = row?;
                    if existing.contains(&node.id) || (!test_query(query) && is_test(&node.file)) {
                        continue;
                    }
                    compound
                        .entry(node.id.clone())
                        .or_insert_with(|| (node.clone(), 0))
                        .1 += 1;
                    let lower = node.name.to_lowercase();
                    let boundary = lower.find(&term).is_some_and(|position| {
                        position > 0
                            && node
                                .name
                                .as_bytes()
                                .get(position)
                                .is_some_and(u8::is_ascii_uppercase)
                    });
                    if boundary {
                        let score = 8.0
                            + (6.0 - node.name.len().saturating_sub(term.len()) as f64 / 4.0)
                                .max(0.0)
                            + path_rank.score(&node.file);
                        candidates.push(Hit { node, score });
                    }
                }
            }
            rank(&mut candidates);
            for hit in candidates.into_iter().take(16) {
                camel.entry(hit.node.id.clone()).or_insert((hit, 0)).1 += 1;
            }
        }
        let mut camel: Vec<_> = camel
            .into_values()
            .map(|(mut hit, count)| {
                hit.score = hit.score * (1 + count) as f64 + count.saturating_sub(1) as f64 * 30.0;
                hit
            })
            .collect();
        deterministic_rank(&mut camel);
        camel.truncate(SEARCH_LIMIT);
        let camel_ids: HashSet<_> = camel.iter().map(|hit| &hit.node.id).collect();
        let mut compounds: Vec<_> = compound
            .into_values()
            .filter(|(node, count)| *count >= 2 && !camel_ids.contains(&node.id))
            .map(|(node, count)| Hit {
                score: 10.0
                    + (count - 1) as f64 * 20.0
                    + path_rank.score(&node.file)
                    + (6.0 - node.name.len() as f64 / 8.0).max(0.0),
                node,
            })
            .collect();
        deterministic_rank(&mut compounds);
        hits.extend(camel);
        hits.extend(compounds.into_iter().take(4));
        Ok(())
    }

    fn gather(&self, roots: &[Hit], max_nodes: usize) -> Result<Vec<(Node, usize)>> {
        let mut seen: HashSet<_> = roots.iter().map(|hit| hit.node.id.clone()).collect();
        let mut nodes: Vec<_> = roots.iter().map(|hit| (hit.node.clone(), 0)).collect();
        let hierarchy_limit = max_nodes / 4;
        for root in roots
            .iter()
            .filter(|hit| TYPES.contains(&hit.node.kind.as_str()))
        {
            for (node, depth) in self.traverse(
                &root.node,
                2,
                hierarchy_limit.max(1),
                BOTH,
                HIERARCHY,
                TYPES,
            )? {
                if nodes.len() >= roots.len() + hierarchy_limit {
                    break;
                }
                if seen.insert(node.id.clone()) {
                    nodes.push((node, depth));
                }
            }
        }
        let per_root = max_nodes.div_ceil(roots.len().max(1));
        for root in roots {
            for (node, level) in self.traverse(&root.node, 3, per_root, BOTH, &[], CONTEXT)? {
                if nodes.len() >= max_nodes {
                    break;
                }
                if seen.insert(node.id.clone()) {
                    nodes.push((node, level));
                }
            }
        }
        let mut per_file = HashMap::new();
        nodes.retain(|(node, _)| {
            let n = per_file.entry(node.file.clone()).or_insert(0);
            *n += 1;
            *n <= (max_nodes / 5).max(5)
        });
        Ok(nodes)
    }

    fn graph_rank(&self, nodes: &[(Node, usize)], roots: &HashSet<String>) -> Result<Vec<f64>> {
        let index: HashMap<_, _> = nodes
            .iter()
            .enumerate()
            .map(|(i, (node, _))| (node.id.as_str(), i))
            .collect();
        let mut adjacent = vec![Vec::new(); nodes.len()];
        let count = index.keys().filter(|&&id| roots.contains(id)).count();
        let mut base = vec![0.0; nodes.len()];
        for (i, (node, _)) in nodes.iter().enumerate() {
            if count == 0 || roots.contains(&node.id) {
                base[i] = 1.0 / if count == 0 { nodes.len() } else { count } as f64;
            }
            for edge in self.neighbors(&node.id, Direction::Out, RANK_EDGES, 200)? {
                if let Some(&j) = index.get(edge.node.id.as_str()).filter(|&&j| i != j) {
                    adjacent[i].push(j);
                    adjacent[j].push(i);
                }
            }
        }
        for list in &mut adjacent {
            list.sort_unstable();
            list.dedup();
        }
        let mut mass = base.clone();
        for _ in 0..25 {
            let mut next: Vec<_> = base.iter().map(|p| p * 0.25).collect();
            for (i, links) in adjacent.iter().enumerate() {
                if links.is_empty() {
                    next[i] += mass[i] * 0.75;
                }
                for &j in links {
                    next[j] += mass[i] * 0.75 / links.len() as f64;
                }
            }
            mass = next;
        }
        Ok(mass)
    }

    fn call_paths(&self, nodes: &[(Node, usize)], roots: &HashSet<String>) -> Result<String> {
        let index: HashMap<_, _> = nodes
            .iter()
            .enumerate()
            .map(|(i, (node, _))| (node.id.as_str(), i))
            .collect();
        let mut routes = Vec::new();
        for (start, _) in nodes
            .iter()
            .enumerate()
            .filter(|(_, (node, _))| roots.contains(&node.id))
        {
            let mut queue = VecDeque::from([vec![(start, None)]]);
            let mut visited = HashSet::from([start]);
            while let Some(path) = queue.pop_front() {
                let Some(&(tail, _)) = path.last() else {
                    continue;
                };
                if path.len() == 4 {
                    continue;
                }
                for call in self.neighbors(&nodes[tail].0.id, Direction::Out, CALLS, 60)? {
                    let Some(&next) = index.get(call.node.id.as_str()) else {
                        continue;
                    };
                    if !visited.insert(next) {
                        continue;
                    }
                    let mut path = path.clone();
                    if let Some(last) = path.last_mut() {
                        last.1 = call.line;
                    }
                    path.push((next, None));
                    if roots.contains(&call.node.id) {
                        routes.push(
                            path.iter()
                                .map(|&(i, line)| {
                                    let node = &nodes[i].0;
                                    format!(
                                        "{} ({}:{})",
                                        node.name,
                                        node.file,
                                        line.unwrap_or(node.start)
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join(" → "),
                        );
                        if routes.len() >= 8 {
                            break;
                        }
                    }
                    queue.push_back(path);
                }
                if routes.len() >= 8 {
                    break;
                }
            }
            if routes.len() >= 8 {
                break;
            }
        }
        if routes.is_empty() {
            return Ok(String::new());
        }
        Ok(format!("\n**Call paths**\n- {}\n", routes.join("\n- ")))
    }

    pub(super) fn explore_tool(
        &self,
        session: &mut ExploreSession,
        args: &Value,
    ) -> Result<String> {
        let query = input(args, "query").ok_or(QueryError::Invalid("Provide a query."))?;
        let (mut groups, mut terms_query) = (BTreeMap::<String, FileContext>::new(), Vec::new());
        for token in query.split_whitespace().take(32) {
            let token =
                token.trim_matches(|c| matches!(c, '`' | '"' | '\'' | '(' | ')' | ',' | '?'));
            let (hint, start, end) = file_anchor(token);
            if hint.contains('.') || hint.contains('/') {
                if let [file] = self.file_matches(hint)?.as_slice() {
                    let pin = groups.len();
                    let group = groups.entry(file.clone()).or_default();
                    group.pin.get_or_insert(pin);
                    if let Some(start) = start {
                        group
                            .anchors
                            .push((start, end.unwrap_or(start.saturating_add(12)).max(start)));
                    }
                    continue;
                }
            }
            terms_query.push(token);
        }
        let match_query = terms_query.join(" ");
        let roots = self.roots(&match_query)?;
        let mut nodes = self.gather(&roots, NODE_CAP)?;
        let mut root_ids: HashSet<_> = roots.iter().map(|hit| hit.node.id.clone()).collect();
        let mut known: HashSet<_> = nodes.iter().map(|(node, _)| node.id.clone()).collect();
        let mut named = HashSet::new();
        let prose = terms_query.len() > 1 && query.ends_with('?');
        for token in terms_query.iter().filter(|token| {
            token.len() >= 2
                && (!prose || distinctive(token) || token.contains("::") || token.contains('.'))
        }) {
            let mut defs = self.definitions(token, None)?;
            defs.retain(|node| !node.generated && (test_query(query) || !is_test(&node.file)));
            defs.sort_by_key(|node| std::cmp::Reverse(node.end.saturating_sub(node.start)));
            if defs.len() > 3 && prose {
                continue;
            }
            for node in defs.into_iter().take(3) {
                named.insert(node.id.clone());
                root_ids.insert(node.id.clone());
                if known.insert(node.id.clone()) {
                    nodes.push((node, 0));
                }
            }
        }
        for (file, group) in &mut groups {
            let file_nodes = self.file_nodes(file, NODE_CAP)?;
            for node in file_nodes.into_iter().filter(Node::definition).take(40) {
                if known.insert(node.id.clone()) {
                    nodes.push((node.clone(), 0));
                }
                group.nodes.push((node, 3.0));
            }
        }
        // Admit graph glue only in files already surfaced by traversal.
        let surfaced: HashSet<_> = nodes.iter().map(|(node, _)| node.file.clone()).collect();
        let mut glue = HashSet::new();
        for hit in &roots {
            for direction in BOTH {
                for next in self.neighbors(&hit.node.id, *direction, CALLS, GLUE_CAP)? {
                    if glue.len() >= GLUE_CAP {
                        break;
                    }
                    if surfaced.contains(&next.node.file) && known.insert(next.node.id.clone()) {
                        glue.insert(next.node.id.clone());
                        nodes.push((next.node, 1));
                    }
                }
            }
        }
        if nodes.is_empty() && groups.is_empty() {
            return Ok(format!("No relevant code found for \"{query}\". Try literal names or paths from the index."));
        }
        let rank = self.graph_rank(&nodes, &root_ids)?;
        let query_terms = terms(&match_query, false);
        let path_rank = PathRank::new(&match_query);
        for ((node, level), mass) in nodes.iter().zip(rank) {
            let group = groups.entry(node.file.clone()).or_default();
            let explicitly_named = named.contains(&node.id);
            let is_root = root_ids.contains(&node.id);
            group.named |= explicitly_named;
            group.entry |= is_root;
            let importance = if explicitly_named {
                30.0
            } else if is_root {
                15.0
            } else if glue.contains(&node.id) {
                8.0
            } else {
                4.0 / (*level + 1) as f64
            };
            let penalty = if node.generated || (!test_query(query) && is_test(&node.file)) {
                0.2
            } else {
                1.0
            };
            group.weight += mass * penalty;
            group.score += importance * penalty;
            if let Some((_, rank)) = group
                .nodes
                .iter_mut()
                .find(|(known, _)| known.id == node.id)
            {
                *rank = rank.max(importance);
            } else {
                group.nodes.push((node.clone(), importance));
            }
        }
        let mut files: Vec<_> = groups.into_iter().collect();
        let max_mass = files
            .iter()
            .map(|(_, group)| group.weight)
            .fold(0.0, f64::max);
        for (path, group) in &mut files {
            let hay = format!(
                "{} {}",
                path,
                group
                    .nodes
                    .iter()
                    .map(|(n, _)| n.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .to_lowercase();
            group.terms = query_terms
                .iter()
                .filter(|term| hay.contains(*term))
                .count();
            group.score += path_rank.score(path);
        }
        let keep = |file: &(String, FileContext)| {
            let (path, group) = file;
            group.pin.is_some()
                || (test_query(query) || !is_test(path))
                    && (group.entry || group.weight >= max_mass * 0.06 || group.terms >= 2)
        };
        if files.iter().filter(|file| keep(file)).count() >= 2 {
            files.retain(keep);
        }
        files.sort_by(|a, b| {
            let (a, b) = (&a.1, &b.1);
            a.pin
                .unwrap_or(usize::MAX)
                .cmp(&b.pin.unwrap_or(usize::MAX))
                .then(b.named.cmp(&a.named))
                .then((b.entry && b.terms >= 2).cmp(&(a.entry && a.terms >= 2)))
                .then(b.weight.total_cmp(&a.weight))
                .then(b.terms.cmp(&a.terms))
                .then(b.score.total_cmp(&a.score))
        });
        let file_count: usize = self
            .db
            .query_row("SELECT count(*) FROM files", [], |row| row.get(0))?;
        let (output_cap, default_max, per_file_cap): (usize, i64, usize) = if file_count < 150 {
            (13_000, 4, 3_800)
        } else if file_count < 500 {
            (18_000, 5, 3_800)
        } else if file_count < 5_000 {
            (24_000, 8, 6_500)
        } else {
            (24_000, 8, 7_000)
        };
        let max = number(args, "maxFiles", default_max, 20);
        let paths = self.call_paths(&nodes, &root_ids)?;
        let mut text = String::from("**Relevant context**\n");
        if paths.len() < 4000 {
            text.push_str(&paths);
        }
        let chosen = max.min(files.len());
        let file_budget =
            (output_cap.saturating_sub(text.len()) / chosen.max(1)).clamp(900, per_file_cap);
        for (file, mut group) in files.into_iter().take(chosen) {
            if text.len() + file.len() + 1000 > output_cap {
                break;
            }
            let (count, dependents) = self.dependents(&file)?;
            let _ = writeln!(text, "\n### {file}");
            if count > 0 {
                let _ = writeln!(
                    text,
                    "Blast radius: {count} other indexed files ({})",
                    dependents
                        .iter()
                        .take(3)
                        .map(|path| &path[..path.floor_char_boundary(140)])
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            group
                .nodes
                .sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.start.cmp(&b.0.start)));
            group.nodes.truncate(16);
            let names = group
                .nodes
                .iter()
                .map(|(node, _)| node.name.as_str())
                .filter(|name| name.len() <= 128)
                .collect::<Vec<_>>();
            if !names.is_empty() && names.iter().map(|name| name.len() + 2).sum::<usize>() < 400 {
                let _ = writeln!(text, "Symbols: {}", names.join(", "));
            }
            let Some((source, fresh)) = self.source(&file)? else {
                text.push_str("Source unavailable.\n");
                continue;
            };
            let lines: Vec<_> = source.split('\n').collect();
            let whole_fits = source.len() + lines.len() * 6 < file_budget;
            let mut spans = group.anchors;
            if spans.is_empty() && whole_fits {
                spans.push((1, lines.len()));
            } else if spans.is_empty() && !fresh {
                text.push_str("File changed since indexing. Use codegraph_node file mode for current lines.\n");
                continue;
            } else if spans.is_empty() {
                spans.extend(
                    group
                        .nodes
                        .iter()
                        .map(|(node, _)| (node.start.max(1), node.end)),
                );
            }
            let budget = file_budget.min(output_cap.saturating_sub(text.len() + 150));
            render_source(&mut text, &file, &source, &lines, &spans, budget, session);
        }
        Ok(text)
    }
}

fn deterministic_rank(hits: &mut [Hit]) {
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.node.file.cmp(&b.node.file))
            .then(a.node.start.cmp(&b.node.start))
            .then(a.node.id.cmp(&b.node.id))
    });
}

fn render_source(
    text: &mut String,
    file: &str,
    source: &str,
    lines: &[&str],
    spans: &[(usize, usize)],
    budget: usize,
    session: &mut ExploreSession,
) {
    let served = session.ranges(file, source);
    let mut emitted = Vec::new();
    let mut covered = Vec::new();
    let mut body = String::new();
    let mut pending = false;
    for &(start, end) in spans {
        let end = end.min(lines.len());
        if start > end {
            continue;
        }
        covered.extend(served.iter().filter_map(|&(a, b)| {
            let overlap = (a.max(start), b.min(end));
            (overlap.0 <= overlap.1).then_some(overlap)
        }));
        let mut cursor = start;
        let mut seen = served.clone();
        seen.extend(&emitted);
        merge_spans(&mut seen);
        for (a, b) in seen {
            if b < cursor || a > end {
                continue;
            }
            if cursor < a {
                let (slice, last) =
                    numbered(lines, cursor, a - 1, budget.saturating_sub(body.len()));
                body.push_str(&slice);
                if last >= cursor {
                    emitted.push((cursor, last));
                }
                if last < a - 1 {
                    pending = true;
                }
            }
            cursor = b.saturating_add(1);
        }
        if cursor <= end {
            let (slice, last) = numbered(lines, cursor, end, budget.saturating_sub(body.len()));
            body.push_str(&slice);
            if last >= cursor {
                emitted.push((cursor, last));
            }
            if last < end {
                pending = true;
            }
        }
    }
    if !body.is_empty() {
        let _ = write!(text, "\n```text\n{body}```\n");
    }
    if !covered.is_empty() {
        merge_spans(&mut covered);
        let ranges = covered
            .into_iter()
            .take(6)
            .map(|(a, b)| format!("{a}-{b}"))
            .collect::<Vec<_>>();
        let _ = writeln!(text, "Previously shown source: {}.", ranges.join(", "));
    }
    if pending {
        text.push_str("More source is indexed. Use a file:line range to continue.\n");
    }
    served.extend(emitted);
    merge_spans(served);
}
