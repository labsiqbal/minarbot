//! Adapted from colbymchenry/codegraph v1.6.2 (6560052), src/resolution/
//! path-aliases.ts, go-module.ts, workspace-packages.ts, and
//! frameworks/cargo-workspace.ts. License: MIT (Copyright (c) 2026 Colby Mchenry).

use super::dirname;
use crate::scan::read_text;
use globset::GlobBuilder;
use serde_json::Value;
use std::{
    cmp::Reverse,
    collections::{BTreeSet, HashMap, HashSet},
    path::{Component, Path, PathBuf},
    sync::LazyLock,
};
use toml_edit::DocumentMut;

#[derive(Default)]
pub(super) struct Config {
    pub(super) rust_manifest: bool,
    pub(super) express: bool,
    pub(super) django: bool,
    pub(super) flask: bool,
    pub(super) go_module: Option<String>,
    pub(super) crates: HashMap<String, String>,
    pub(super) cargo_roots: HashSet<String>,
    pub(super) cargo_dependencies: HashSet<String>,
    pub(super) packages: HashMap<String, String>,
    manifests: HashMap<String, Value>,
    aliases: HashMap<String, AliasMap>,
}

impl Config {
    pub(super) fn load<'a>(root: &Path, files: impl Iterator<Item = &'a str>) -> Self {
        let mut dirs = BTreeSet::from([""]);
        for file in files {
            if relative(root, &root.join(file)).as_deref() != Some(file) {
                continue;
            }
            let mut dir = dirname(file);
            while dirs.insert(dir) {
                dir = dirname(dir);
            }
        }
        let mut config = Self {
            go_module: go_module(root),
            ..Self::default()
        };
        config.django = root.join("manage.py").is_file();
        for name in ["requirements.txt", "pyproject.toml", "Pipfile", "setup.py"] {
            if let Some(text) = read_text(&root.join(name)) {
                let words = text.split(|c: char| !c.is_ascii_alphanumeric() && c != '_');
                for word in words {
                    config.django |= word.eq_ignore_ascii_case("django");
                    config.flask |= word.eq_ignore_ascii_case("flask");
                }
            }
        }
        let cargo = manifest(root, "Cargo.toml");
        config.rust_manifest = cargo.is_some();
        let cargo_members = cargo
            .as_ref()
            .and_then(|cargo| cargo.get("workspace"))
            .and_then(|workspace| workspace.get("members"))
            .and_then(toml_edit::Item::as_array);
        let cargo_globs = globs(
            cargo_members
                .into_iter()
                .flatten()
                .filter_map(toml_edit::Value::as_str),
        );
        let package = read_json(&root.join("package.json"));
        let workspace = package.as_ref().and_then(|pkg| pkg.get("workspaces"));
        let package_members =
            workspace.and_then(|ws| ws.as_array().or_else(|| ws.get("packages")?.as_array()));
        let pnpm = read_text(&root.join("pnpm-workspace.yaml")).unwrap_or_default();
        let package_globs = globs(
            package_members
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .chain(pnpm_packages(&pnpm)),
        );
        for dir in dirs {
            let base = root.join(dir);
            if let Some(alias) = load_aliases(root, &base) {
                config.aliases.insert(dir.to_owned(), alias);
            }
            if let Some(pkg) = read_json(&base.join("package.json")) {
                if let Some(name) = pkg.get("name").and_then(Value::as_str) {
                    if package_globs.iter().any(|glob| glob.is_match(dir)) {
                        config.packages.insert(name.to_owned(), dir.to_owned());
                    }
                }
                const EXPRESS: &[&str] = &["express", "koa", "fastify", "hapi", "@hapi/hapi"];
                config.express |= pkg
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| EXPRESS.contains(&name))
                    || ["dependencies", "devDependencies", "peerDependencies"]
                        .iter()
                        .any(|key| {
                            pkg.get(key).is_some_and(|deps| {
                                EXPRESS.iter().any(|name| deps.get(*name).is_some())
                            })
                        });
                config.manifests.insert(dir.to_owned(), pkg);
            }
            if let Some(local) = manifest(&base, "Cargo.toml") {
                collect_dependencies(local.as_table(), &mut config.cargo_dependencies);
                if let Some(targets) = local.get("target").and_then(toml_edit::Item::as_table) {
                    for target in targets.iter().filter_map(|(_, target)| target.as_table()) {
                        collect_dependencies(target, &mut config.cargo_dependencies);
                    }
                }
                if let Some(name) = local
                    .get("package")
                    .and_then(|pkg| pkg.get("name"))
                    .and_then(toml_edit::Item::as_str)
                {
                    config.cargo_roots.insert(name.replace('-', "_"));
                    if cargo_globs.iter().any(|glob| glob.is_match(dir)) {
                        config.crates.insert(name.to_owned(), dir.to_owned());
                        config.crates.insert(name.replace('-', "_"), dir.to_owned());
                    }
                }
            }
        }
        config
            .cargo_dependencies
            .retain(|name| !config.cargo_roots.contains(name));
        config
    }

    pub(super) fn depends_on(&self, file: &str, matches: impl Fn(&str) -> bool) -> bool {
        let mut dir = dirname(file);
        loop {
            if let Some(pkg) = self.manifests.get(dir) {
                let own = pkg
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(&matches);
                let declares = [
                    "dependencies",
                    "devDependencies",
                    "peerDependencies",
                    "optionalDependencies",
                ]
                .into_iter()
                .filter_map(|key| pkg.get(key).and_then(Value::as_object))
                .any(|deps| deps.keys().any(|name| matches(name)));
                if own || declares {
                    return true;
                }
            }
            if dir.is_empty() {
                return false;
            }
            dir = dirname(dir);
        }
    }

    pub(super) fn alias_paths(&self, root: &Path, file: &str, name: &str) -> Vec<String> {
        let mut dir = dirname(file);
        let mut nearest = None;
        while !dir.is_empty() {
            if let Some(aliases) = self.aliases.get(dir) {
                nearest = Some(aliases);
                break;
            }
            dir = dirname(dir);
        }
        nearest
            .into_iter()
            .chain(self.aliases.get(""))
            .flat_map(|aliases| aliases.apply(root, name))
            .collect()
    }

    pub(super) fn package_path(&self, specifier: &str) -> Option<String> {
        let (package, dir) = self
            .packages
            .iter()
            .filter(|(name, _)| {
                specifier == *name
                    || specifier
                        .strip_prefix(name.as_str())
                        .is_some_and(|suffix| suffix.starts_with('/'))
            })
            .max_by_key(|(name, _)| name.len())?;
        Some(format!("{dir}{}", &specifier[package.len()..]))
    }

    pub(super) fn external_package(&self, file: &str, specifier: &str) -> bool {
        if specifier.starts_with(['.', '/']) || self.package_path(specifier).is_some() {
            return false;
        }
        let segments = if specifier.starts_with('@') { 2 } else { 1 };
        let pkg = specifier
            .splitn(segments + 1, '/')
            .take(segments)
            .collect::<Vec<_>>()
            .join("/");
        if specifier.starts_with("@std/")
            || ["node:", "bun:", "jsr:", "npm:", "http:", "https:"]
                .iter()
                .any(|prefix| specifier.starts_with(prefix))
            || NODE_BUILTINS.contains(&specifier)
        {
            return true;
        }
        let mut dir = dirname(file);
        let mut dependency = false;
        loop {
            if let Some(manifest) = self.manifests.get(dir) {
                if manifest.get("name").and_then(Value::as_str) == Some(pkg.as_str()) {
                    return false;
                }
                for field in [
                    "dependencies",
                    "devDependencies",
                    "peerDependencies",
                    "optionalDependencies",
                ] {
                    if let Some(version) = manifest
                        .get(field)
                        .and_then(|deps| deps.get(&pkg))
                        .and_then(Value::as_str)
                    {
                        if ["workspace:", "file:", "link:", "portal:"]
                            .iter()
                            .any(|prefix| version.starts_with(prefix))
                        {
                            return false;
                        }
                        dependency = true;
                    }
                }
            }
            if dir.is_empty() {
                return dependency;
            }
            dir = dirname(dir);
        }
    }
}

fn collect_dependencies(table: &toml_edit::Table, names: &mut HashSet<String>) {
    for kind in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(deps) = table.get(kind).and_then(toml_edit::Item::as_table_like) {
            names.extend(deps.iter().map(|(name, _)| name.replace('-', "_")));
        }
    }
}

const NODE_BUILTINS: &[&str] = &[
    "assert",
    "assert/strict",
    "async_hooks",
    "buffer",
    "child_process",
    "cluster",
    "console",
    "constants",
    "crypto",
    "dgram",
    "diagnostics_channel",
    "dns",
    "dns/promises",
    "domain",
    "events",
    "fs",
    "fs/promises",
    "http",
    "http2",
    "https",
    "inspector",
    "module",
    "net",
    "os",
    "path",
    "path/posix",
    "path/win32",
    "perf_hooks",
    "process",
    "punycode",
    "querystring",
    "readline",
    "readline/promises",
    "repl",
    "stream",
    "stream/consumers",
    "stream/promises",
    "stream/web",
    "string_decoder",
    "sys",
    "timers",
    "timers/promises",
    "tls",
    "trace_events",
    "tty",
    "url",
    "util",
    "util/types",
    "v8",
    "vm",
    "wasi",
    "worker_threads",
    "zlib",
];

fn manifest(dir: &Path, name: &str) -> Option<DocumentMut> {
    read_text(&dir.join(name))?.parse().ok()
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&read_text(path)?).ok()
}

fn go_module(root: &Path) -> Option<String> {
    static MODULE: LazyLock<regex::Regex> =
        LazyLock::new(|| crate::literal_regex(r"(?m)^[ \t]*module[ \t]+(\S+)[ \t]*$"));
    let source = read_text(&root.join("go.mod"))?;
    for line in source.lines() {
        let code = line
            .split_once("//")
            .map_or(line, |(code, _)| code)
            .trim_end();
        if let Some(capture) = MODULE.captures(code) {
            return Some(capture[1].trim_matches(['"', '\'']).to_owned())
                .filter(|module| !module.is_empty());
        }
    }
    None
}

fn globs<'a>(patterns: impl Iterator<Item = &'a str>) -> Vec<globset::GlobMatcher> {
    patterns
        .filter_map(|pattern| {
            GlobBuilder::new(pattern.trim_end_matches('/'))
                .literal_separator(true)
                .build()
                .ok()
                .map(|glob| glob.compile_matcher())
        })
        .collect()
}

fn pnpm_packages(source: &str) -> impl Iterator<Item = &str> {
    source
        .lines()
        .skip_while(|line| line.trim() != "packages:")
        .skip(1)
        .take_while(|line| {
            line.trim().is_empty() || line.starts_with(char::is_whitespace) || line.starts_with('-')
        })
        .filter_map(|line| {
            line.trim()
                .strip_prefix('-')
                .map(|item| item.trim().trim_matches(['"', '\'']))
        })
}

struct AliasMap {
    base: PathBuf,
    patterns: Vec<(String, String, bool, Vec<String>)>,
}

#[derive(Default)]
struct Options {
    base: Option<PathBuf>,
    paths: Option<(PathBuf, serde_json::Map<String, Value>)>,
}

fn load_aliases(root: &Path, directory: &Path) -> Option<AliasMap> {
    for name in ["tsconfig.json", "jsconfig.json", "tsconfig.base.json"] {
        let Some(options) = effective(root, directory.join(name), &mut HashSet::new(), 0) else {
            continue;
        };
        let Some((dir, paths)) = options.paths else {
            continue;
        };
        let mut patterns: Vec<_> = paths
            .into_iter()
            .filter_map(|(pattern, targets)| {
                let replacements = targets
                    .as_array()?
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if replacements.is_empty() {
                    return None;
                }
                let (prefix, suffix, wildcard) = pattern
                    .split_once('*')
                    .map_or((pattern.as_str(), "", false), |(prefix, suffix)| {
                        (prefix, suffix, true)
                    });
                Some((prefix.to_owned(), suffix.to_owned(), wildcard, replacements))
            })
            .collect();
        patterns.sort_by_key(|(prefix, _, wildcard, _)| (Reverse(prefix.len()), *wildcard));
        if !patterns.is_empty() {
            return Some(AliasMap {
                base: options.base.unwrap_or(dir),
                patterns,
            });
        }
    }
    None
}

fn effective(
    root: &Path,
    path: PathBuf,
    stack: &mut HashSet<PathBuf>,
    depth: usize,
) -> Option<Options> {
    let path = normalize(&path)?;
    if depth > 32 || !path.starts_with(root) || stack.contains(&path) {
        return None;
    }
    let dir = path.parent()?;
    let data: Value = serde_json::from_str(&jsonc(&read_text(&path)?)).ok()?;
    stack.insert(path.clone());
    let mut options = Options::default();
    if let Some(extends) = data.get("extends") {
        let parents = extends
            .as_array()
            .map_or_else(|| vec![extends], |list| list.iter().collect());
        for name in parents.into_iter().filter_map(Value::as_str) {
            let paths = if name.starts_with('.') || Path::new(name).is_absolute() {
                vec![dir.join(name)]
            } else {
                dir.ancestors()
                    .take_while(|parent| parent.starts_with(root))
                    .map(|parent| parent.join("node_modules").join(name))
                    .collect()
            };
            for possible in paths.into_iter().flat_map(|base| {
                let json = base.with_file_name(format!(
                    "{}.json",
                    base.file_name().unwrap_or_default().to_string_lossy()
                ));
                [base.clone(), json, base.join("tsconfig.json")]
            }) {
                if let Some(parent) = effective(root, possible, stack, depth + 1) {
                    options.base = parent.base.or(options.base);
                    options.paths = parent.paths.or(options.paths);
                    break;
                }
            }
        }
    }
    stack.remove(&path);
    if let Some(compiler) = data.get("compilerOptions") {
        if let Some(base) = compiler.get("baseUrl").and_then(Value::as_str) {
            options.base = normalize(&dir.join(base));
        }
        if let Some(paths) = compiler.get("paths").and_then(Value::as_object) {
            options.paths = Some((dir.to_owned(), paths.clone()));
        }
    }
    Some(options)
}

impl AliasMap {
    fn apply(&self, root: &Path, name: &str) -> Vec<String> {
        for (prefix, suffix, wildcard, replacements) in &self.patterns {
            let Some(rest) = name
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_suffix(suffix))
            else {
                continue;
            };
            if !wildcard && !rest.is_empty() {
                continue;
            }
            return replacements
                .iter()
                .filter_map(|replacement| {
                    let target = replacement.replacen('*', rest, usize::from(*wildcard));
                    relative(root, &self.base.join(target))
                })
                .collect();
        }
        Vec::new()
    }
}

pub(super) fn relative(root: &Path, path: &Path) -> Option<String> {
    let normalized = normalize(path)?;
    Some(
        normalized
            .strip_prefix(root)
            .ok()?
            .to_str()?
            .replace('\\', "/"),
    )
}

fn normalize(path: &Path) -> Option<PathBuf> {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => (),
            Component::ParentDir => {
                if !output.pop() {
                    return None;
                }
            }
            part => output.push(part),
        }
    }
    Some(output)
}

fn jsonc(source: &str) -> String {
    let mut chars = source.trim_start_matches('\u{feff}').chars().peekable();
    let mut clean = String::new();
    let mut in_string = false;
    let mut comma = None;
    while let Some(c) = chars.next() {
        if in_string {
            clean.push(c);
            if c == '\\' {
                clean.extend(chars.next());
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '/' && chars.next_if_eq(&'/').is_some() {
            clean.extend(chars.by_ref().find(|&c| c == '\n'));
            continue;
        }
        if c == '/' && chars.next_if_eq(&'*').is_some() {
            while let Some(c) = chars.next() {
                if c == '*' && chars.next_if_eq(&'/').is_some() {
                    break;
                }
            }
            clean.push(' ');
            continue;
        }
        if c == '"' {
            in_string = true;
        }
        if !c.is_whitespace() {
            if matches!(c, '}' | ']') {
                if let Some(index) = comma {
                    clean.remove(index);
                }
            }
            comma = (c == ',').then_some(clean.len());
        }
        clean.push(c);
    }
    clean
}
