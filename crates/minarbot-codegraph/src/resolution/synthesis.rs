//! Adapted from colbymchenry/codegraph v1.6.2 (6560052), src/resolution/
//! callback-synthesizer.ts and synth-utils.ts.
//! License: MIT (Copyright (c) 2026 Colby Mchenry).

use super::{dirname, js, type_kind, Resolver};
use rusqlite::{params, Connection, Statement};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::LazyLock,
};

const IMPLEMENTATION_CAP: usize = 40;
const EVENT_CAP: usize = 6;

pub(super) fn refresh(db: &Connection, resolver: &mut Resolver<'_>) -> rusqlite::Result<()> {
    db.execute(
        "DELETE FROM edges WHERE CASE WHEN json_valid(metadata) THEN json_extract(metadata, '$.synthesizedBy') END IN
         ('go-method-contains', 'go-implements', 'interface-impl', 'event-emitter', 'celery-dispatch',
          'react-render', 'jsx-render', 'callback')",
        [],
    )?;
    let mut methods = HashMap::<usize, Vec<usize>>::new();
    let mut attached = HashSet::new();
    let mut statement = db.prepare(
        "SELECT e.source, e.target FROM edges e
         JOIN nodes src ON e.source = src.id JOIN nodes dst ON e.target = dst.id
         WHERE e.kind = 'contains' AND dst.kind = 'method'
           AND src.kind IN ('class', 'struct', 'union', 'trait', 'interface', 'enum', 'type_alias')
         ORDER BY e.kind, e.target, e.line, e.col",
    )?;
    for edge in statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        let (source, target) = edge?;
        if let (Some(&source), Some(&target)) =
            (resolver.ids.get(&source), resolver.ids.get(&target))
        {
            methods.entry(source).or_default().push(target);
            attached.insert(target);
        }
    }
    let mut writer = Writer {
        resolver,
        insert: db.prepare(
            "INSERT OR IGNORE INTO edges (source, target, kind, line, metadata, provenance)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?,
    };
    let implicit = writer.go_methods(&mut methods, &attached)?;
    writer.callbacks(&methods, &implicit)?;
    writer.field_callbacks(db)?;
    writer.react(&methods)?;
    writer.dispatches()
}

struct Writer<'a, 'db, 'root> {
    resolver: &'a mut Resolver<'root>,
    insert: Statement<'db>,
}

impl Writer<'_, '_, '_> {
    fn edge(
        &mut self,
        endpoints: (usize, usize),
        kind: &str,
        line: Option<u32>,
        metadata: Value,
        heuristic: bool,
    ) -> rusqlite::Result<()> {
        let (source, target) = endpoints;
        self.insert.execute(params![
            self.resolver.symbols[source].id,
            self.resolver.symbols[target].id,
            kind,
            line,
            metadata.to_string(),
            heuristic.then_some("heuristic"),
        ])?;
        Ok(())
    }

    fn go_methods(
        &mut self,
        methods: &mut HashMap<usize, Vec<usize>>,
        attached: &HashSet<usize>,
    ) -> rusqlite::Result<BTreeMap<usize, Vec<usize>>> {
        let mut go_structs = Vec::new();
        let mut interfaces = Vec::new();
        for id in 0..self.resolver.symbols.len() {
            let symbol = &self.resolver.symbols[id];
            if symbol.language != "go" {
                continue;
            }
            match symbol.kind.as_str() {
                "method" if !attached.contains(&id) => {
                    let owner = symbol.owner().and_then(|name| {
                        self.resolver.named(name).iter().copied().find(|&id| {
                            let n = &self.resolver.symbols[id];
                            n.language == "go"
                                && type_kind(&n.kind)
                                && dirname(&n.file) == dirname(&symbol.file)
                        })
                    });
                    if let Some(owner) = owner {
                        methods.entry(owner).or_default().push(id);
                        self.edge(
                            (owner, id),
                            "contains",
                            Some(symbol.start),
                            json!({"synthesizedBy": "go-method-contains"}),
                            false,
                        )?;
                    }
                }
                "interface" => interfaces.push(id),
                "struct" => go_structs.push(id),
                _ => {}
            }
        }
        let method_names = |ids: &[usize]| {
            ids.iter()
                .map(|&id| self.resolver.symbols[id].name.clone())
                .collect::<HashSet<_>>()
        };
        let names = methods
            .iter()
            .map(|(&owner, ids)| (owner, method_names(ids)))
            .collect::<HashMap<_, _>>();
        let mut implicit = BTreeMap::<usize, Vec<usize>>::new();
        for interface in interfaces {
            let Some(want) = names.get(&interface).filter(|want| !want.is_empty()) else {
                continue;
            };
            let matches = go_structs
                .iter()
                .copied()
                .filter(|id| names.get(id).is_some_and(|have| want.is_subset(have)))
                .take(IMPLEMENTATION_CAP);
            for id in matches {
                let symbol = &self.resolver.symbols[id];
                let site = format!("{}:{}", symbol.file, symbol.start);
                let name = self.resolver.symbols[interface].name.clone();
                implicit.entry(id).or_default().push(interface);
                self.edge(
                    (id, interface),
                    "implements",
                    Some(symbol.start),
                    json!({
                        "synthesizedBy": "go-implements", "via": name, "registeredAt": site,
                    }),
                    true,
                )?;
            }
        }
        Ok(implicit)
    }

    fn callbacks(
        &mut self,
        methods: &HashMap<usize, Vec<usize>>,
        implicit: &BTreeMap<usize, Vec<usize>>,
    ) -> rusqlite::Result<()> {
        let mut seen = HashSet::new();
        for id in 0..self.resolver.symbols.len() {
            let node = &self.resolver.symbols[id];
            if !matches!(node.kind.as_str(), "class" | "struct" | "union")
                || !override_language(&node.language)
            {
                continue;
            }
            let Some(implementations) = methods.get(&id) else {
                continue;
            };
            let parents = self
                .resolver
                .supers
                .get(&id)
                .into_iter()
                .flatten()
                .chain(implicit.get(&id).into_iter().flatten())
                .copied()
                .collect::<Vec<_>>();
            for parent in parents.into_iter().filter(|&parent| parent != id) {
                let Some(base) = methods
                    .get(&parent)
                    .filter(|_| override_language(&self.resolver.symbols[parent].language))
                else {
                    continue;
                };
                let mut added = 0;
                for &origin in base {
                    for &target in implementations {
                        if added == IMPLEMENTATION_CAP {
                            break;
                        }
                        let source = &self.resolver.symbols[origin];
                        let candidate = &self.resolver.symbols[target];
                        if origin == target
                            || source.name != candidate.name
                            || !seen.insert((origin, target))
                        {
                            continue;
                        }
                        let site = format!("{}:{}", candidate.file, candidate.start);
                        let name = candidate.name.clone();
                        self.edge((origin, target), "calls", Some(source.start), json!({
                            "synthesizedBy": "interface-impl", "via": name, "registeredAt": site,
                        }), true)?;
                        added += 1;
                    }
                }
            }
        }
        Ok(())
    }

    fn field_callbacks(&mut self, db: &Connection) -> rusqlite::Result<()> {
        static REG: LazyLock<regex::Regex> = LazyLock::new(|| {
            crate::literal_regex(
                r"^(?:on[A-Z]\w*|subscribe|addListener|addEventListener|register|watch|listen|addCallback)$",
            )
        });
        static DISP: LazyLock<regex::Regex> = LazyLock::new(|| {
            crate::literal_regex(r"(?i)emit|trigger|notify|dispatch|fire|publish|flush")
        });
        static STORAGE: LazyLock<regex::Regex> =
            LazyLock::new(|| crate::literal_regex(r"this\.(\w+)\.(?:add|push|set)\("));
        static NOTIFY: LazyLock<regex::Regex> = LazyLock::new(|| {
            crate::literal_regex(r"\bof\s+(?:Array\.from\(\s*)?this\.(\w+)|this\.(\w+)\.forEach\(")
        });
        let mut registrars = Vec::new();
        let mut dispatchers = HashMap::<(String, String), Vec<usize>>::new();
        for id in 0..self.resolver.symbols.len() {
            let n = &self.resolver.symbols[id];
            if !js(&n.language) || !matches!(n.kind.as_str(), "method" | "function") {
                continue;
            }
            let reg = REG.is_match(&n.name);
            let disp = DISP.is_match(&n.name);
            if !reg && !disp {
                continue;
            }
            let (file, lang, start, end) = (n.file.clone(), n.language.clone(), n.start, n.end);
            let module = self.resolver.module(&file, &lang);
            let text = module.range(start, end);
            if reg {
                if let Some(field) = STORAGE.captures(text).and_then(|m| m.get(1)) {
                    registrars.push((id, file.clone(), field.as_str().to_owned()));
                }
            }
            if disp {
                if let Some(field) = NOTIFY
                    .captures(text)
                    .and_then(|m| m.get(1).or_else(|| m.get(2)))
                {
                    dispatchers
                        .entry((file, field.as_str().to_owned()))
                        .or_default()
                        .push(id);
                }
            }
        }
        let mut calls = db.prepare_cached("SELECT source, line FROM edges WHERE target = ? AND kind = 'calls' AND line IS NOT NULL ORDER BY id")?;
        let mut references = db.prepare_cached(
            "SELECT target, json_extract(metadata, '$.resolvedBy') FROM edges
             WHERE source = ? AND kind = 'references' AND line = ?
               AND CASE WHEN json_valid(metadata) THEN json_extract(metadata, '$.fnRef') END
               AND CASE WHEN json_valid(metadata) THEN json_extract(metadata, '$.refName') END = ?",
        )?;
        let mut seen = HashSet::new();
        for (id, file, field) in registrars {
            let Some(dispatchers) = dispatchers.get(&(file, field.clone())) else {
                continue;
            };
            let registrar = &self.resolver.symbols[id];
            let name = registrar.name.clone();
            let argument = crate::literal_regex(&format!(
                r"\b{}\s*\(\s*((?:this\.)?\w+)\s*[,)]",
                regex::escape(&name)
            ));
            let mut added = 0;
            for row in calls.query_map([&registrar.id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
            })? {
                if added == IMPLEMENTATION_CAP {
                    break;
                }
                let (caller, line) = row?;
                let Some(&source) = self.resolver.ids.get(&caller) else {
                    continue;
                };
                let n = &self.resolver.symbols[source];
                let (file, lang) = (n.file.clone(), n.language.clone());
                let module = self.resolver.module(&file, &lang);
                let Some(binding) = argument.captures(module.line(line)).and_then(|m| m.get(1))
                else {
                    continue;
                };
                let targets = references
                    .query_map(params![caller, line, binding.as_str()], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let [(target, by)] = targets.as_slice() else {
                    continue;
                };
                let Some(&target) = self.resolver.ids.get(target) else {
                    continue;
                };
                let node = &self.resolver.symbols[target];
                if !matches!(node.kind.as_str(), "function" | "method")
                    || (!binding.as_str().starts_with("this.")
                        && node.file != file
                        && by != "import")
                {
                    continue;
                }
                for &dispatcher in dispatchers {
                    if added == IMPLEMENTATION_CAP {
                        break;
                    }
                    if dispatcher != target && seen.insert((dispatcher, target)) {
                        self.edge((dispatcher, target), "calls", Some(self.resolver.symbols[dispatcher].start), json!({
                            "synthesizedBy": "callback", "via": name, "field": field, "registeredAt": format!("{file}:{line}"),
                        }), true)?;
                        added += 1;
                    }
                }
            }
        }
        Ok(())
    }

    fn react(&mut self, methods: &HashMap<usize, Vec<usize>>) -> rusqlite::Result<()> {
        static SET_STATE: LazyLock<regex::Regex> =
            LazyLock::new(|| crate::literal_regex(r"this\.setState\s*\("));
        static TAG: LazyLock<regex::Regex> =
            LazyLock::new(|| crate::literal_regex(r"<([A-Z][A-Za-z0-9_]*)[\s/>]"));
        let mut classes = methods.keys().copied().collect::<Vec<_>>();
        classes.sort_unstable();
        for owner in classes {
            let class = &self.resolver.symbols[owner];
            if class.kind != "class" || !js(&class.language) {
                continue;
            }
            let members = &methods[&owner];
            let Some(render) = members
                .iter()
                .copied()
                .find(|&id| self.resolver.symbols[id].name == "render")
            else {
                continue;
            };
            let (file, lang) = (class.file.clone(), class.language.clone());
            let registered = format!("{file}:{}", self.resolver.symbols[render].start);
            let module = self.resolver.module(&file, &lang);
            let dispatchers = members
                .iter()
                .copied()
                .filter(|&id| {
                    let method = &self.resolver.symbols[id];
                    id != render && SET_STATE.is_match(module.range(method.start, method.end))
                })
                .take(IMPLEMENTATION_CAP)
                .collect::<Vec<_>>();
            for id in dispatchers {
                self.edge((id, render), "calls", Some(self.resolver.symbols[id].start), json!({
                    "synthesizedBy": "react-render", "via": "setState", "registeredAt": registered,
                }), true)?;
            }
        }
        let mut files = self.resolver.files.keys().cloned().collect::<Vec<_>>();
        files.sort();
        for file in files {
            let Some(file_id) = self.resolver.file_node(&file) else {
                continue;
            };
            let language = self.resolver.symbols[file_id].language.clone();
            if !js(&language) {
                continue;
            }
            let module = self.resolver.module(&file, &language);
            if !looks_like_jsx(&module.text) {
                continue;
            }
            let parents = self.resolver.in_file(&file).to_vec();
            for id in parents {
                let n = &self.resolver.symbols[id];
                if !matches!(n.kind.as_str(), "method" | "function" | "component") {
                    continue;
                }
                let start = n.start;
                let text = module.range(start, n.end);
                if !looks_like_jsx(text) {
                    continue;
                }
                let mut names = HashSet::new();
                let mut targets = HashSet::new();
                for captures in TAG.captures_iter(text) {
                    let name = captures[1].to_owned();
                    if !names.insert(name.clone()) {
                        continue;
                    }
                    let candidates = self
                        .resolver
                        .named(&name)
                        .iter()
                        .copied()
                        .filter(|&id| {
                            let n = &self.resolver.symbols[id];
                            js(&n.language)
                                && matches!(n.kind.as_str(), "component" | "function" | "class")
                        })
                        .collect::<Vec<_>>();
                    let hit = match candidates.as_slice() {
                        [only] => Some(*only),
                        _ => {
                            let local = candidates
                                .iter()
                                .copied()
                                .find(|&id| self.resolver.symbols[id].file == file);
                            let mapping = module.imports.iter().find(|imp| imp.local == name);
                            local
                                .or_else(|| {
                                    let mapping = mapping?;
                                    let path = self.resolver.import_path(
                                        &mapping.source,
                                        &file,
                                        &language,
                                    )?;
                                    let imported = candidates
                                        .iter()
                                        .copied()
                                        .find(|&id| self.resolver.symbols[id].file == path);
                                    if imported.is_some()
                                        || !candidates.is_empty()
                                        || mapping.name != "default"
                                    {
                                        return imported;
                                    }
                                    let mut components =
                                        self.resolver.in_file(&path).iter().copied().filter(
                                            |&id| self.resolver.symbols[id].kind == "component",
                                        );
                                    let component = components.next()?;
                                    components.next().is_none().then_some(component)
                                })
                                .or_else(|| candidates.first().copied())
                        }
                    };
                    if let Some(target) =
                        hit.filter(|&target| target != id && targets.insert(target))
                    {
                        self.edge(
                            (id, target),
                            "calls",
                            Some(start),
                            json!({
                                "synthesizedBy": "jsx-render", "via": name,
                            }),
                            true,
                        )?;
                        if targets.len() == 30 {
                            break;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn dispatches(&mut self) -> rusqlite::Result<()> {
        static EMIT: LazyLock<regex::Regex> = LazyLock::new(|| {
            crate::literal_regex(r#"\.(?:emit|fire|dispatchEvent)\(\s*['"]([^'"]+)['"]"#)
        });
        static ON: LazyLock<regex::Regex> = LazyLock::new(|| {
            crate::literal_regex(
                r#"\.(?:on|once|addListener)\(\s*['"]([^'"]+)['"]\s*,\s*(?:function\s+(\w+)|(?:this\.)?(\w+))"#,
            )
        });
        static CELERY: LazyLock<regex::Regex> = LazyLock::new(|| {
            crate::literal_regex(r"\b([A-Za-z_]\w*)\s*\.\s*(?:delay|apply_async)\s*\(")
        });
        let mut files = self.resolver.files.keys().cloned().collect::<Vec<_>>();
        files.sort();
        let mut emits = BTreeMap::<String, BTreeSet<usize>>::new();
        let mut handlers = BTreeMap::<String, BTreeMap<usize, String>>::new();
        let mut celery = HashSet::new();
        for file in &files {
            let Some(file_id) = self.resolver.file_node(file) else {
                continue;
            };
            let lang = self.resolver.symbols[file_id].language.clone();
            if !js(&lang) && lang != "python" {
                continue;
            }
            let module = self.resolver.module(file, &lang);
            for captures in EMIT.captures_iter(&module.text).filter(|_| js(&lang)) {
                if let Some(found) = captures.get(0) {
                    let line = module
                        .lines
                        .partition_point(|&start| start <= found.start())
                        as u32;
                    if let Some(source) = self.enclosing_function(file, line) {
                        emits
                            .entry(captures[1].to_owned())
                            .or_default()
                            .insert(source);
                    }
                }
            }
            for captures in ON.captures_iter(&module.text).filter(|_| js(&lang)) {
                if let (Some(found), Some(binding)) =
                    (captures.get(0), captures.get(2).or_else(|| captures.get(3)))
                {
                    let line = module
                        .lines
                        .partition_point(|&start| start <= found.start());
                    if let Some(target) = self
                        .resolver
                        .named(binding.as_str())
                        .iter()
                        .copied()
                        .find(|&id| {
                            let node = &self.resolver.symbols[id];
                            js(&node.language)
                                && matches!(node.kind.as_str(), "function" | "method")
                        })
                    {
                        handlers
                            .entry(captures[1].to_owned())
                            .or_default()
                            .insert(target, format!("{file}:{line}"));
                    }
                }
            }
            for captures in CELERY
                .captures_iter(&module.text)
                .filter(|_| lang == "python")
                .take(80)
            {
                let Some(found) = captures.get(0) else {
                    continue;
                };
                let line = module
                    .lines
                    .partition_point(|&start| start <= found.start())
                    as u32;
                if module.line(line).trim_start().starts_with('#') {
                    continue;
                }
                let Some(source) = self.enclosing_function(file, line) else {
                    continue;
                };
                let name = &captures[1];
                let candidates = self.resolver.named(name).to_vec();
                let tasks = candidates
                    .into_iter()
                    .filter(|&id| self.celery_task(id))
                    .collect::<Vec<_>>();
                let task = match tasks.as_slice() {
                    [id] => Some(*id),
                    _ => tasks
                        .into_iter()
                        .find(|&id| self.resolver.symbols[id].file == *file),
                };
                if let Some(target) =
                    task.filter(|&target| source != target && celery.insert((source, target)))
                {
                    self.edge((source, target), "calls", Some(line), json!({
                       "synthesizedBy": "celery-dispatch", "via": name, "registeredAt": format!("{file}:{line}"),
                    }), true)?;
                }
            }
        }
        let mut seen = HashSet::new();
        for (event, dispatchers) in emits {
            let Some(handlers) = handlers
                .get(&event)
                .filter(|map| map.len() <= EVENT_CAP && dispatchers.len() <= EVENT_CAP)
            else {
                continue;
            };
            for source in dispatchers {
                for (&target, registered) in handlers {
                    if source != target && seen.insert((source, target)) {
                        self.edge((source, target), "calls", None, json!({
                            "synthesizedBy": "event-emitter", "event": event, "registeredAt": registered,
                        }), true)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn enclosing_function(&self, file: &str, line: u32) -> Option<usize> {
        self.resolver
            .in_file(file)
            .iter()
            .copied()
            .filter(|&id| {
                let n = &self.resolver.symbols[id];
                matches!(n.kind.as_str(), "function" | "method") && n.start <= line && line <= n.end
            })
            .max_by_key(|&id| self.resolver.symbols[id].start)
    }

    fn celery_task(&mut self, id: usize) -> bool {
        static DECORATOR: LazyLock<regex::Regex> = LazyLock::new(|| {
            crate::literal_regex(r"@\s*(?:[A-Za-z_][\w.]*\.)?(?:shared_task|task)\b")
        });
        let n = &self.resolver.symbols[id];
        if n.kind != "function" || n.language != "python" {
            return false;
        }
        let (file, start) = (n.file.clone(), n.start);
        let module = self.resolver.module(&file, "python");
        (start.saturating_sub(12).max(1)..start)
            .rev()
            .map(|line| module.line(line).trim())
            .take_while(|line| {
                !["def ", "async def ", "class "]
                    .iter()
                    .any(|prefix| line.starts_with(prefix))
            })
            .any(|line| DECORATOR.is_match(line))
    }
}

fn override_language(language: &str) -> bool {
    matches!(language, "rust" | "go" | "typescript" | "javascript")
}

fn looks_like_jsx(source: &str) -> bool {
    source.contains("</") || source.contains("/>")
}
