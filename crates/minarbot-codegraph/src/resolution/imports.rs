//! Adapted from colbymchenry/codegraph v1.6.2 (6560052), src/resolution/
//! import-resolver.ts and alias-binding.ts.
//! License: MIT (Copyright (c) 2026 Colby Mchenry).

use super::{config::relative, dirname, family, js, type_kind, Match, Reference, Resolver};
use std::{collections::HashSet, path::Path, sync::LazyLock};

macro_rules! re {
    ($pattern:literal) => {{
        static RE: LazyLock<regex::Regex> = LazyLock::new(|| crate::literal_regex($pattern));
        &*RE
    }};
}

#[derive(Debug)]
pub(super) struct Import {
    pub(super) local: String,
    pub(super) name: String,
    pub(super) source: String,
}

impl Import {
    fn new(local: &str, name: &str, source: &str) -> Self {
        Self {
            local: local.to_owned(),
            name: name.to_owned(),
            source: source.to_owned(),
        }
    }
}

pub(super) struct Module {
    pub(super) text: String,
    pub(super) lines: Vec<usize>,
    pub(super) imports: Vec<Import>,
    forwards: Vec<Import>,
    pub(super) rust_uses: Vec<Import>,
    pub(super) sealed: bool,
    pub(super) esm: bool,
}

impl Module {
    pub(super) fn new(text: String, language: &str) -> Self {
        let mut module = Self {
            lines: crate::textutil::line_starts(&text),
            text,
            imports: Vec::new(),
            forwards: Vec::new(),
            rust_uses: Vec::new(),
            sealed: false,
            esm: false,
        };
        if js(language) {
            module.extract_js();
        } else if language == "python" {
            module.extract_python();
        } else if language == "go" {
            module.extract_go();
        } else if language == "rust" {
            for capture in re!(r"(?:^|[;{}\s])use\s+([^;]{1,2000});").captures_iter(&module.text) {
                rust_use_tree(capture[1].trim(), "", &mut module.rust_uses, 0);
            }
        }
        module
    }

    pub(super) fn line(&self, line: u32) -> &str {
        let Some(start) = line
            .checked_sub(1)
            .and_then(|n| self.lines.get(n as usize))
            .copied()
        else {
            return "";
        };
        let end = self
            .lines
            .get(line as usize)
            .copied()
            .unwrap_or(self.text.len());
        self.text[start..end].trim_end_matches(['\n', '\r'])
    }

    pub(super) fn range(&self, start: u32, end: u32) -> &str {
        let start = start
            .checked_sub(1)
            .and_then(|line| self.lines.get(line as usize))
            .copied();
        let end = self
            .lines
            .get(end as usize)
            .copied()
            .unwrap_or(self.text.len());
        start
            .and_then(|start| self.text.get(start..end))
            .unwrap_or_default()
    }

    pub(super) fn at(&self, line: u32, column: u32) -> &str {
        let line = self.line(line);
        let mut offset = 0;
        for (byte, ch) in line.char_indices() {
            if offset == column {
                return &line[byte..];
            }
            offset += ch.len_utf16() as u32;
        }
        ""
    }

    pub(super) fn export_local(&self, name: &str) -> bool {
        self.forwards
            .iter()
            .any(|imp| imp.source.is_empty() && imp.name == name)
    }

    fn extract_js(&mut self) {
        let mut text = self.text.as_str();
        // Ignore comments between named specifiers without touching quoted paths.
        let scratch;
        if text.contains("/*") || text.contains("//") {
            scratch = re!(r#"(?s)"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|(?m://[^\n]*)|/\*.*?\*/"#)
                .replace_all(text, |capture: &regex::Captures<'_>| {
                    if capture[0].starts_with('/') {
                        " ".to_owned()
                    } else {
                        capture[0].to_owned()
                    }
                });
            text = &scratch;
        }
        let imports = re!(r"(?m)^\s*import\s+[^\s(]").is_match(text);
        let exports = re!(r"(?m)^\s*export\b").is_match(text);
        let common_js = text.contains("module.exports") || text.contains("exports.");
        self.esm = (imports || exports) && !common_js && !text.contains("declare global");
        self.sealed = imports && !exports && !common_js;
        let statements = re!(r#"\bimport\s+([\w$*,{}\s]+?)\s+from\s*['"]([^'"]+)['"]"#);
        for capture in statements.captures_iter(text) {
            let head = capture[1].trim();
            let head = head
                .strip_prefix("type")
                .filter(|rest| rest.starts_with(char::is_whitespace))
                .map_or(head, str::trim_start);
            if let Some((prefix, list)) = head.split_once('{') {
                let primary = prefix.trim().trim_end_matches(',').trim();
                if !primary.is_empty() {
                    self.imports
                        .push(Import::new(primary, "default", &capture[2]));
                }
                self.imports
                    .extend(named(list.trim_end_matches('}'), &capture[2], "as", false));
            } else if let Some((prefix, namespace)) = head.split_once('*') {
                let primary = prefix.trim().trim_end_matches(',').trim();
                if !primary.is_empty() {
                    self.imports
                        .push(Import::new(primary, "default", &capture[2]));
                }
                if let Some(alias) = namespace.trim().strip_prefix("as ") {
                    self.imports
                        .push(Import::new(alias.trim(), "*", &capture[2]));
                }
            } else {
                self.imports.push(Import::new(head, "default", &capture[2]));
            }
        }
        let require = re!(
            r#"(?:\b(?:const|let|var)\s+|,\s*)(?:([A-Za-z_$][\w$]*)|\{([^}]+)\})\s*=\s*require\(\s*['"]([^'"]+)['"]\s*\)(?:\s*\.\s*([A-Za-z_$][\w$]*))?"#
        );
        for capture in require.captures_iter(text) {
            if let Some(local) = capture.get(1) {
                self.imports.push(Import::new(
                    local.as_str(),
                    capture.get(4).map_or("default", |m| m.as_str()),
                    &capture[3],
                ));
            } else if let Some(list) = capture.get(2) {
                for item in list
                    .as_str()
                    .split(',')
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                {
                    let (name, local) = item.split_once(':').unwrap_or((item, item));
                    self.imports
                        .push(Import::new(local.trim(), name.trim(), &capture[3]));
                }
            }
        }
        for capture in re!(r#"\bexport\s*\*\s*(?:as\s+([\w$]+)\s*)?from\s*['"]([^'"]+)['"]"#)
            .captures_iter(text)
        {
            self.forwards.push(Import::new(
                capture.get(1).map_or("*", |m| m.as_str()),
                "*",
                &capture[2],
            ));
        }
        for capture in
            re!(r#"\bexport\s*\{([^}]+)\}(?:\s*from\s*['"]([^'"]+)['"])?"#).captures_iter(text)
        {
            self.forwards.extend(named(
                &capture[1],
                capture.get(2).map_or("", |m| m.as_str()),
                "as",
                false,
            ));
        }
        if let Some(capture) =
            re!(r"(?m)^[ \t]*export\s+default\s+([A-Za-z_$][\w$]*)\s*;?[ \t]*$").captures(text)
        {
            self.forwards.push(Import::new("default", &capture[1], ""));
        }
        if let Some(capture) = re!(r#"(?m)^[ \t]*(?:exports\s*=\s*)?module\.exports\s*=\s*(?:exports\s*=\s*)?(?:require\(\s*['"]([^'"]+)['"]\s*\)\s*;?[ \t]*$|(?:async\s+)?function\s*\*?\s*([A-Za-z_$][\w$]*)\s*\(|class\s+([A-Za-z_$][\w$]*)|([A-Za-z_$][\w$]*)\s*;?[ \t]*$)"#).captures(text) {
            if let Some(source) = capture.get(1) {
                self.forwards.push(Import::new("default", "default", source.as_str()));
            } else if let Some(binding) = (2..=4).find_map(|index| capture.get(index)) {
                self.forwards.push(Import::new("default", binding.as_str(), ""));
            }
        }
        for capture in
            re!(r"(?m)^[ \t]*(?:module\.)?exports\.([\w$]+)[ \t]*=[ \t]*([\w$]+)\s*;?[ \t]*$")
                .captures_iter(text)
        {
            self.forwards
                .push(Import::new(&capture[1], &capture[2], ""));
        }
        for capture in re!(r"(?m)^[ \t]*(?:module\.)?exports\.([\w$]+)\s*=\s*(?:(?:async\s+)?function(?:\s*\*)?\s*([\w$]*)\s*\(|class\s*([\w$]*)\b)").captures_iter(text) {
            let exported = &capture[1];
            let local = capture.get(2).or_else(|| capture.get(3)).map_or(exported, |m| m.as_str());
            self.forwards.push(Import::new(exported, if local.is_empty() { exported } else { local }, ""));
        }
    }

    fn extract_python(&mut self) {
        for capture in
            re!(r"\bfrom\s+([\w.]+)\s+import\s+(\([^)]*\)|[^#\n]+)").captures_iter(&self.text)
        {
            let list = capture[2].trim().trim_matches(['(', ')']);
            let clean = re!(r"#[^\n]*").replace_all(list, "");
            self.imports.extend(named(&clean, &capture[1], "as", true));
        }
        for capture in re!(r"(?m)^import\s+([\w.]+)(?:\s+as\s+(\w+))?").captures_iter(&self.text) {
            let source = &capture[1];
            let local = capture.get(2).map_or_else(
                || source.rsplit('.').next().unwrap_or(source),
                |m| m.as_str(),
            );
            self.imports.push(Import::new(local, "*", source));
        }
    }

    fn extract_go(&mut self) {
        for capture in re!(r#"\bimport\s+(?:(\w+)\s+)?["']([^"']+)["']"#).captures_iter(&self.text)
        {
            self.imports.push(go_binding(&capture));
        }
        for block in re!(r"(?s)\bimport\s*\(([^)]+)\)").captures_iter(&self.text) {
            for capture in re!(r#"(?:(\w+)\s+)?["']([^"']+)["']"#).captures_iter(&block[1]) {
                self.imports.push(go_binding(&capture));
            }
        }
    }
}

fn named(list: &str, source: &str, alias: &str, stars: bool) -> Vec<Import> {
    let mut imports = Vec::new();
    for item in list.split(',').map(str::trim) {
        let parts = item.split_whitespace().collect::<Vec<_>>();
        let (name, local) = match parts.as_slice() {
            ["type", name, "as", local] => (*name, *local),
            [name, marker, local] if *marker == alias => (*name, *local),
            ["type", name] => (*name, *name),
            [name] if *name != "*" || stars => (*name, *name),
            _ => continue,
        };
        imports.push(Import::new(local, name, source));
    }
    imports
}

fn js_path_specifier(name: &str) -> bool {
    matches!(name, "." | "..")
        || name.starts_with("./")
        || name.starts_with("../")
        || (name.contains('/')
            && !name.contains(char::is_whitespace)
            && !re!(r"^@[\w.-]+/[\w.-]+$").is_match(name))
}

fn go_binding(capture: &regex::Captures<'_>) -> Import {
    let source = &capture[2];
    let local = capture.get(1).map_or_else(
        || source.rsplit('/').next().unwrap_or(source),
        |m| m.as_str(),
    );
    Import::new(local, "*", source)
}

fn rust_use_tree(tree: &str, prefix: &str, uses: &mut Vec<Import>, depth: usize) {
    if depth > 32 {
        return;
    }
    if let Some((head, list)) = tree.split_once('{') {
        let head = format!("{prefix}{}", head.trim());
        let mut nesting = 0;
        let mut start = 0;
        for (index, token) in list.char_indices() {
            match token {
                '{' => nesting += 1,
                '}' if nesting > 0 => nesting -= 1,
                ',' | '}' if nesting == 0 => {
                    rust_use_tree(list[start..index].trim(), &head, uses, depth + 1);
                    start = index + 1;
                }
                _ => {}
            }
        }
        return;
    }
    if tree.is_empty() {
        return;
    }
    let (path, local) = tree
        .split_once(" as ")
        .map_or((tree, None), |(path, alias)| (path, Some(alias)));
    let path = if path == "self" {
        prefix.trim_end_matches("::").to_owned()
    } else {
        format!("{prefix}{path}")
    };
    let name = path.rsplit("::").next().unwrap_or(&path).trim();
    uses.push(Import::new(local.unwrap_or(name).trim(), name, &path));
}

impl Resolver<'_> {
    pub(super) fn import_path(&self, name: &str, from: &str, language: &str) -> Option<String> {
        let extensions: &[&str] = match language {
            "typescript" => &[
                ".ts",
                ".tsx",
                ".d.ts",
                ".js",
                ".jsx",
                "/index.ts",
                "/index.tsx",
                "/index.js",
            ],
            "tsx" => &[
                ".tsx",
                ".ts",
                ".d.ts",
                ".js",
                ".jsx",
                "/index.tsx",
                "/index.ts",
                "/index.js",
            ],
            "javascript" => &[".js", ".jsx", ".mjs", ".cjs", "/index.js", "/index.jsx"],
            "jsx" => &[".jsx", ".js", "/index.jsx", "/index.js"],
            "python" => &[".py", "/__init__.py"],
            _ => return None,
        };
        let dir = self.root.join(dirname(from));
        let mut bases = Vec::new();
        if name.starts_with('.') {
            let mut path = dir;
            if language == "python" {
                let rest = name.trim_start_matches('.');
                for _ in 1..(name.len() - rest.len()) {
                    path.pop();
                }
                path.push(rest.replace('.', "/"));
            } else {
                path.push(name);
            }
            bases.push(relative(self.root, &path)?);
        } else if js(language) {
            bases = self.config.alias_paths(self.root, from, name);
            if let Some(path) = self.config.package_path(name) {
                bases.push(path);
            }
            for (prefix, dir) in [
                ("@/", "src/"),
                ("~/", "src/"),
                ("@src/", "src/"),
                ("src/", "src/"),
                ("@app/", "app/"),
                ("app/", "app/"),
            ] {
                if let Some(rest) = name.strip_prefix(prefix) {
                    bases.push(format!("{dir}{rest}"));
                }
            }
            if name.starts_with('/') {
                if let Some(path) = relative(self.root, Path::new(name)) {
                    bases.push(path);
                }
                bases.push(name.trim_start_matches('/').to_owned());
            }
        } else if language == "python" {
            return self.python_file(name, from);
        }
        for base in bases {
            for extension in extensions {
                let path = if base.is_empty() {
                    extension.trim_start_matches('/').to_owned()
                } else {
                    format!("{base}{extension}")
                };
                if self.files.contains_key(&path) {
                    return Some(path);
                }
            }
            if self.files.contains_key(&base) {
                return Some(base);
            }
            if js(language) {
                for (emitted, source) in [
                    (".js", &[".ts", ".tsx", ".d.ts"][..]),
                    (".jsx", &[".tsx"]),
                    (".mjs", &[".mts", ".d.mts"]),
                    (".cjs", &[".cts", ".d.cts"]),
                ] {
                    if let Some(stem) = base.strip_suffix(emitted) {
                        for extension in source {
                            let path = format!("{stem}{extension}");
                            if self.files.contains_key(&path) {
                                return Some(path);
                            }
                        }
                    }
                }
            }
        }
        None
    }

    fn python_file(&self, name: &str, from: &str) -> Option<String> {
        if name.is_empty() || name.starts_with('.') {
            return None;
        }
        let path = name.replace('.', "/");
        let leaf = name.rsplit('.').next()?;
        for (basename, suffix) in [
            (format!("{leaf}.py"), format!("{path}.py")),
            ("__init__.py".to_owned(), format!("{path}/__init__.py")),
        ] {
            if let Some(symbol) = self
                .named(&basename)
                .iter()
                .map(|&id| &self.symbols[id])
                .find(|n| {
                    n.kind == "file"
                        && n.file != from
                        && (n.file == suffix
                            || n.file
                                .strip_suffix(&suffix)
                                .is_some_and(|prefix| prefix.ends_with('/')))
                })
            {
                return Some(symbol.file.clone());
            }
        }
        None
    }

    fn exported(
        &mut self,
        path: &str,
        name: &str,
        language: &str,
        visited: &mut HashSet<(String, String)>,
        depth: usize,
    ) -> Option<usize> {
        let python = language == "python";
        if depth > if python { 3 } else { 8 } || !visited.insert((path.to_owned(), name.to_owned()))
        {
            return None;
        }
        let module = self.module(path, language);
        let local = |resolver: &Self, name: &str, needs_export: bool| {
            resolver.named(name).iter().copied().find(|&id| {
                let n = &resolver.symbols[id];
                n.file == path
                    && n.kind != "import"
                    && n.kind != "file"
                    && (!needs_export || n.exported)
                    && (!python || n.owner().is_none())
            })
        };
        if name == "default" && !python {
            if let Some(target) = module
                .forwards
                .iter()
                .filter(|imp| imp.local == "default" && imp.source.is_empty())
                .find_map(|imp| local(self, &imp.name, false))
            {
                return Some(target);
            }
            if let Some(target) = self.in_file(path).iter().copied().find(|&id| {
                let n = &self.symbols[id];
                n.exported && matches!(n.kind.as_str(), "class" | "function")
            }) {
                return Some(target);
            }
        } else if let Some(target) = local(self, name, !python) {
            return Some(target);
        }
        let forwards = if python {
            &module.imports
        } else {
            &module.forwards
        };
        for import in forwards.iter().filter(|imp| imp.local == name) {
            if import.source.is_empty() {
                if let Some(target) = local(self, &import.name, false) {
                    return Some(target);
                }
            } else if let Some(next) = self.import_path(&import.source, path, language) {
                if let Some(target) =
                    self.exported(&next, &import.name, language, visited, depth + 1)
                {
                    return Some(target);
                }
            }
        }
        if !python {
            if let Some((head, rest)) = name.split_once('.') {
                if let Some(import) = forwards
                    .iter()
                    .find(|imp| imp.local == head && imp.name == "*")
                {
                    let next = self.import_path(&import.source, path, language)?;
                    return self.exported(&next, rest, language, visited, depth + 1);
                }
            }
        }
        for import in forwards.iter().filter(|imp| imp.local == "*") {
            if let Some(next) = self.import_path(&import.source, path, language) {
                if let Some(target) = self.exported(&next, name, language, visited, depth + 1) {
                    return Some(target);
                }
            }
        }
        None
    }

    pub(super) fn via_import(&mut self, reference: &Reference, module: &Module) -> Option<Match> {
        let language = reference.language.as_str();
        let name = reference.name.as_str();
        let from = reference.file.as_str();
        let importing = reference.kind == "imports";
        if importing
            && (js(language) && js_path_specifier(name)
                || (language == "python" && name.contains('.') && !name.starts_with('.')))
        {
            if let Some(file) = self
                .import_path(name, from, language)
                .filter(|path| path != from)
                .and_then(|path| self.file_node(&path))
            {
                return Some(Match::new(file, 0.9, "import"));
            }
        }
        if language == "rust" {
            return self.rust_import(reference, module);
        }
        let (root, member) = name
            .split_once('.')
            .map_or((name, None), |(root, member)| (root, Some(member)));
        for mapping in module.imports.iter().filter(|imp| imp.local == root) {
            if language == "go" {
                let Some(member) = member else { continue };
                let Some(dir) = self.go_package(&mapping.source) else {
                    continue;
                };
                if let Some(&id) = self.named(member).iter().find(|&&id| {
                    let n = &self.symbols[id];
                    n.language == "go" && n.exported && dirname(&n.file) == dir
                }) {
                    return Some(Match::new(id, 0.9, "import"));
                }
                continue;
            }
            if language == "python" {
                let package = if mapping.name == "*" {
                    mapping.source.clone()
                } else {
                    let separator = if mapping.source.ends_with('.') {
                        ""
                    } else {
                        "."
                    };
                    format!("{}{separator}{}", mapping.source, mapping.name)
                };
                if let Some(path) = self
                    .import_path(&package, from, language)
                    .filter(|path| path != from)
                {
                    if importing && member.is_none() {
                        if let Some(file) = self.file_node(&path) {
                            return Some(Match::new(file, 0.9, "import"));
                        }
                    } else if let Some(member) = member.and_then(|member| member.split('.').next())
                    {
                        if let Some(target) =
                            self.exported(&path, member, language, &mut HashSet::new(), 0)
                        {
                            return Some(Match::new(target, 0.85, "import"));
                        }
                    }
                }
            }
            let Some(path) = self.import_path(&mapping.source, from, language) else {
                continue;
            };
            if importing && matches!(mapping.name.as_str(), "*" | "default") && path != from {
                if let Some(file) = self.file_node(&path) {
                    return Some(Match::new(file, 0.9, "import"));
                }
            }
            let wanted = if mapping.name == "*" {
                member?
            } else {
                &mapping.name
            };
            let Some(mut target) = self.exported(&path, wanted, language, &mut HashSet::new(), 0)
            else {
                continue;
            };
            if let Some(member) = member.filter(|_| mapping.name != "*") {
                if let Some(id) = self.imported_member(target, member, reference) {
                    target = id;
                } else if language == "python" {
                    continue;
                }
            }
            return Some(Match::new(target, 0.9, "import"));
        }
        None
    }

    fn imported_member(
        &mut self,
        target: usize,
        member: &str,
        reference: &Reference,
    ) -> Option<usize> {
        let node = &self.symbols[target];
        let path = node.file.clone();
        let owner = node.qualified.clone();
        let (start, end, column, end_column) = (node.start, node.end, node.column, node.end_column);
        if type_kind(&node.kind) {
            let full = format!("{owner}::{}", member.replace('.', "::"));
            return self
                .qualified(&full)
                .iter()
                .copied()
                .find(|&id| self.symbols[id].file == path);
        }
        if !matches!(node.kind.as_str(), "constant" | "variable") {
            return None;
        }
        let member = member.split('.').next()?;
        if let Some(target) = self.named(member).iter().copied().find(|&id| {
            let n = &self.symbols[id];
            n.file == path
                && (n.start, n.column) >= (start, column)
                && (n.end, n.end_column) <= (end, end_column)
                && matches!(n.kind.as_str(), "method" | "function")
        }) {
            return Some(target);
        }
        let module = self.module(&path, &reference.language);
        let line = module.line(start);
        let kind = re!(r"=\s*new\s+([\w$]+)|:\s*([\w$]+)\s*=").captures(line)?;
        let owner = kind.get(1).or_else(|| kind.get(2))?.as_str();
        self.method_on_type(owner, member, reference, false)
    }

    pub(super) fn go_package<'a>(&self, path: &'a str) -> Option<&'a str> {
        let module = self.config.go_module.as_deref()?;
        let rest = path.strip_prefix(module)?;
        if rest.is_empty() {
            Some("")
        } else {
            rest.strip_prefix('/')
        }
    }

    pub(super) fn rust_module(&self, segments: &[&str], from: &str) -> Option<String> {
        let parent = Path::new(dirname(from));
        let crate_dir = parent.ancestors().find(|dir| {
            ["lib.rs", "main.rs"].iter().any(|name| {
                dir.join(name)
                    .to_str()
                    .is_some_and(|path| self.files.contains_key(path))
            })
        });
        let filename = from.rsplit('/').next()?;
        let self_dir = if matches!(filename, "lib.rs" | "main.rs" | "mod.rs") {
            parent.to_owned()
        } else {
            parent.join(filename.strip_suffix(".rs")?)
        };
        let locate = |dir: &Path, rest: &[&str]| {
            let mut dir = dir.to_owned();
            let mut file = None;
            for segment in rest {
                let a = dir.join(format!("{segment}.rs"));
                dir.push(segment);
                let b = dir.join("mod.rs");
                file = [a, b].into_iter().find_map(|path| {
                    let path = path.to_str()?;
                    self.files.contains_key(path).then(|| path.to_owned())
                });
                file.as_ref()?;
            }
            file
        };
        match segments {
            ["crate", rest @ ..] => locate(crate_dir?, rest),
            ["self", rest @ ..] => locate(&self_dir, rest),
            ["super", ..] => {
                let count = segments
                    .iter()
                    .take_while(|&&segment| segment == "super")
                    .count();
                locate(self_dir.ancestors().nth(count)?, &segments[count..])
            }
            [package, rest @ ..] if self.config.crates.contains_key(*package) => {
                let dir = Path::new(&self.config.crates[*package]).join("src");
                if rest.is_empty() {
                    Some(dir.join("lib.rs").to_str()?.to_owned())
                } else {
                    locate(&dir, rest)
                }
            }
            _ => locate(&self_dir, segments).or_else(|| locate(crate_dir?, segments)),
        }
    }

    fn rust_import(&self, reference: &Reference, module: &Module) -> Option<Match> {
        let name = reference.name.as_str();
        if reference.kind == "imports" {
            if let Some(directory) = self.config.crates.get(name) {
                for file in ["src/lib.rs", "src/main.rs"] {
                    if let Some(target) = self.file_node(&format!("{directory}/{file}")) {
                        let mut hit = Match::new(target, 0.95, "framework");
                        hit.framework = Some("rust");
                        return Some(hit);
                    }
                }
            }
        }
        let (path, leaf) = if let Some((path, leaf)) = name.rsplit_once("::") {
            (path, leaf)
        } else {
            let alias = module
                .rust_uses
                .iter()
                .find(|imp| imp.local == name && imp.name != name)?;
            alias.source.rsplit_once("::")?
        };
        let segments = path
            .split("::")
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        let file = self.rust_module(&segments, &reference.file)?;
        if file == reference.file {
            return None;
        }
        let target = self.named(leaf).iter().copied().find(|&id| {
            let n = &self.symbols[id];
            n.file == file
                && family(&n.language, &reference.language)
                && (type_kind(&n.kind)
                    || matches!(n.kind.as_str(), "function" | "method" | "constant"))
        })?;
        Some(Match::new(target, 0.9, "import"))
    }
}
