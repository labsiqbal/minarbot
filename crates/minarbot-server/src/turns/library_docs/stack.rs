use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
use toml_edit::{Document, Item};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Ecosystem {
    Npm,
    Rust,
    Go,
    Python,
    Php,
    Ruby,
    Maven,
    Dart,
    Swift,
    Nuget,
}
use Ecosystem::*;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub(super) struct Library {
    pub ecosystem: Ecosystem,
    pub name: String,
    pub version: Option<String>,
}

pub(super) fn package_name(name: &str) -> bool {
    (1..=180).contains(&name.len())
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && !name.contains("://")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@/._:-+".contains(&b))
        && name.bytes().any(|b| b.is_ascii_alphabetic())
}

pub(super) fn exact(raw: &str) -> Option<&str> {
    let v = raw.strip_prefix('=').unwrap_or(raw).trim();
    let release = v.strip_prefix(['v', 'V']).unwrap_or(v);
    let (base, suffix) = release.split_once(['-', '+', '!']).unwrap_or((release, ""));
    // Accept pinned release/prerelease versions, including PEP 440 and Go pseudo-versions.
    (v.len() <= 96
        && base.starts_with(|c: char| c.is_ascii_digit())
        && base.split('.').all(|part| {
            !part.is_empty()
                && part.starts_with(|c: char| c.is_ascii_digit())
                && part.bytes().all(|b| b.is_ascii_alphanumeric())
                && !part.eq_ignore_ascii_case("x")
        })
        && suffix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
        && !v.ends_with(['-', '+', '!'])
        && !v.contains('*'))
    .then_some(v)
}

fn normalized(ecosystem: Ecosystem, name: &str) -> String {
    if ecosystem == Python {
        name.split(['-', '_', '.'])
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-")
            .to_ascii_lowercase()
    } else if matches!(ecosystem, Nuget | Swift) {
        name.to_ascii_lowercase()
    } else {
        name.to_owned()
    }
}

pub(super) fn name_matches(entry: &Library, name: &str) -> bool {
    entry.name == normalized(entry.ecosystem, name)
}

fn kind(file: &str) -> Option<(Ecosystem, bool)> {
    Some(match file {
        "package.json" => (Npm, false),
        "package-lock.json"
        | "npm-shrinkwrap.json"
        | "pnpm-lock.yaml"
        | "yarn.lock"
        | "bun.lock" => (Npm, true),
        "Cargo.toml" => (Rust, false),
        "Cargo.lock" => (Rust, true),
        "go.mod" => (Go, false),
        "pyproject.toml" => (Python, false),
        "uv.lock" | "poetry.lock" | "Pipfile.lock" => (Python, true),
        "composer.json" => (Php, false),
        "composer.lock" => (Php, true),
        "Gemfile.lock" => (Ruby, true),
        "pom.xml" | "build.gradle" | "build.gradle.kts" => (Maven, false),
        "gradle.lockfile" => (Maven, true),
        "pubspec.yaml" => (Dart, false),
        "pubspec.lock" => (Dart, true),
        "Package.swift" => (Swift, false),
        "Package.resolved" => (Swift, true),
        "packages.lock.json" => (Nuget, true),
        _ if file.ends_with(".csproj") || file == "Directory.Packages.props" => (Nuget, false),
        _ if file == "requirements.txt"
            || file.starts_with("requirements") && file.ends_with(".txt") =>
        {
            (Python, false)
        }
        _ => return None,
    })
}

#[derive(Default)]
struct Dependencies {
    declared: BTreeSet<(String, Option<String>)>,
    locked: BTreeSet<(String, String)>,
}

pub(super) fn scan(root: &Path) -> Vec<Library> {
    let Ok(paths) = minarbot_codegraph::manifest_paths(root, |file| kind(file).is_some()) else {
        return Vec::new();
    };
    let mut groups: BTreeMap<_, Dependencies> = BTreeMap::new();
    let mut bytes_left = 24 * 1024 * 1024;
    for path in paths {
        let (dir, file) = path.rsplit_once('/').unwrap_or(("", &path));
        let Some((ecosystem, lock)) = kind(file) else {
            continue;
        };
        let Ok((_, bytes)) =
            crate::files::workspace::download_bounded(root, &path, bytes_left.min(4 * 1024 * 1024))
        else {
            continue;
        };
        bytes_left -= bytes.len();
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        let group = groups.entry((dir.to_owned(), ecosystem)).or_default();
        parse(file, text, |name, version| {
            if !package_name(name) {
                return;
            }
            let name = normalized(ecosystem, name);
            let version = version
                .and_then(exact)
                .filter(|v| {
                    lock || ecosystem != Npm
                        || v.split(['-', '+'])
                            .next()
                            .is_some_and(|base| base.split('.').count() == 3)
                })
                .map(str::to_owned);
            if lock {
                if let Some(version) = version {
                    group.locked.insert((name, version));
                }
            } else {
                group.declared.insert((name, version));
            }
        });
    }
    let mut stack = BTreeSet::new();
    for ((dir, ecosystem), group) in &groups {
        for (name, declared) in &group.declared {
            if let Some(version) = declared {
                stack.insert(Library {
                    ecosystem: *ecosystem,
                    name: name.clone(),
                    version: Some(version.clone()),
                });
                continue;
            }
            let resolved = Path::new(dir)
                .ancestors()
                .filter_map(|parent| groups.get(&(parent.to_str()?.to_owned(), *ecosystem)))
                .map(|g| {
                    g.locked
                        .range((name.clone(), String::new())..)
                        .take_while(|(n, _)| n == name)
                })
                .find(|matches| matches.clone().next().is_some());
            if let Some(versions) = resolved {
                stack.extend(versions.map(|(_, version)| Library {
                    ecosystem: *ecosystem,
                    name: name.clone(),
                    version: Some(version.clone()),
                }));
            } else {
                stack.insert(Library {
                    ecosystem: *ecosystem,
                    name: name.clone(),
                    version: declared.clone(),
                });
            }
        }
        if group.declared.is_empty() {
            stack.extend(group.locked.iter().map(|(name, version)| Library {
                ecosystem: *ecosystem,
                name: name.clone(),
                version: Some(version.clone()),
            }));
        }
    }
    stack.into_iter().collect()
}

fn item<'a>(root: &'a Item, keys: &[&str]) -> &'a Item {
    keys.iter()
        .try_fold(root, |value, key| value.get(key))
        .unwrap_or(&Item::None)
}

fn toml_entries<'a>(value: &'a Item, mut entry: impl FnMut(&'a str, &'a Item)) {
    if let Some(table) = value.as_table_like() {
        for (name, v) in table.iter() {
            entry(name, v);
        }
    }
}

fn json_entries<'a>(value: &'a Value, mut entry: impl FnMut(&'a str, &'a Value)) {
    for (name, v) in value.as_object().into_iter().flatten() {
        entry(name, v);
    }
}

fn version_regex(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|_| unreachable!("literal manifest regex"))
}

fn requirements(raw: &str, add: &mut impl FnMut(&str, Option<&str>)) {
    use std::sync::LazyLock;
    static REQUIREMENT: LazyLock<Regex> = LazyLock::new(|| {
        version_regex(
            r"^\s*([A-Za-z0-9][A-Za-z0-9_.-]*)(?:\[[^\]]+\])?\s*(?:(?:===|==)\s*([^,;\s#]+)|[<>=!~;]|$)",
        )
    });
    let raw = raw.split('#').next().unwrap_or("").trim();
    if let Some(parts) = REQUIREMENT.captures(raw) {
        add(&parts[1], parts.get(2).map(|v| v.as_str()));
    }
}

fn parse(file: &str, text: &str, mut add: impl FnMut(&str, Option<&str>)) {
    match file {
        "package.json"
        | "package-lock.json"
        | "npm-shrinkwrap.json"
        | "composer.json"
        | "composer.lock"
        | "Package.resolved"
        | "packages.lock.json"
        | "Pipfile.lock" => {
            let Ok(value) = serde_json::from_str::<Value>(text) else {
                return;
            };
            match file {
                "package.json" => {
                    for key in [
                        "dependencies",
                        "devDependencies",
                        "optionalDependencies",
                        "peerDependencies",
                    ] {
                        json_entries(&value[key], |n, v| {
                            let Some(version) = v.as_str() else { return };
                            if let Some(alias) = version.strip_prefix("npm:") {
                                if let Some((name, version)) = alias.rsplit_once('@') {
                                    add(name, Some(version));
                                }
                            } else if !["file:", "link:", "workspace:"]
                                .iter()
                                .any(|prefix| version.starts_with(prefix))
                            {
                                add(n, Some(version));
                            }
                        });
                    }
                }
                "package-lock.json" | "npm-shrinkwrap.json" => {
                    json_entries(&value["packages"], |path, v| {
                        if v["link"] != true {
                            if let Some((_, name)) = path.rsplit_once("node_modules/") {
                                add(v["name"].as_str().unwrap_or(name), v["version"].as_str());
                            }
                        }
                    });
                    if value["packages"].is_null() {
                        let mut queues = vec![&value["dependencies"]];
                        while let Some(deps) = queues.pop() {
                            json_entries(deps, |name, v| {
                                add(name, v["version"].as_str());
                                queues.push(&v["dependencies"]);
                            });
                        }
                    }
                }
                "composer.json" => {
                    for key in ["require", "require-dev"] {
                        json_entries(&value[key], |name, v| {
                            if name.contains('/') {
                                add(name, v.as_str());
                            }
                        });
                    }
                }
                "composer.lock" => {
                    for key in ["packages", "packages-dev"] {
                        for p in value[key].as_array().into_iter().flatten() {
                            if let Some(name) = p["name"].as_str() {
                                add(name, p["version"].as_str());
                            }
                        }
                    }
                }
                "Package.resolved" => {
                    let pins = value["pins"]
                        .as_array()
                        .or_else(|| value["object"]["pins"].as_array());
                    for pin in pins.into_iter().flatten() {
                        let identity = pin["identity"].as_str().or(pin["package"].as_str());
                        if let Some(name) = swift_name(
                            pin["location"]
                                .as_str()
                                .or(pin["repositoryURL"].as_str())
                                .unwrap_or(""),
                        )
                        .or(identity)
                        {
                            add(
                                name,
                                pin["state"]["version"]
                                    .as_str()
                                    .or(pin["state"]["revision"].as_str()),
                            );
                        }
                    }
                }
                "packages.lock.json" => json_entries(&value["dependencies"], |_, v| {
                    json_entries(v, |name, dep| {
                        if dep["type"] != "Project" {
                            add(name, dep["resolved"].as_str());
                        }
                    });
                }),
                "Pipfile.lock" => {
                    for key in ["default", "develop"] {
                        json_entries(&value[key], |n, v| {
                            add(n, v["version"].as_str().and_then(|s| s.strip_prefix("==")))
                        });
                    }
                }
                _ => {}
            }
        }
        "Cargo.toml" | "Cargo.lock" | "pyproject.toml" | "uv.lock" | "poetry.lock" => {
            let Ok(doc) = text.parse::<Document<String>>() else {
                return;
            };
            let doc = doc.as_item();
            match file {
                "Cargo.toml" => {
                    let mut tables = vec![doc, item(doc, &["workspace"])];
                    toml_entries(item(doc, &["target"]), |_, v| tables.push(v));
                    for table in tables {
                        for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
                            toml_entries(item(table, &[key]), |n, v| {
                                if v.get("path").is_none()
                                    && v.get("git").is_none()
                                    && v.get("workspace").is_none()
                                {
                                    let name = v.get("package").and_then(Item::as_str).unwrap_or(n);
                                    let req = v
                                        .as_str()
                                        .or_else(|| v.get("version").and_then(Item::as_str));
                                    add(name, req.and_then(|s| s.strip_prefix('=')));
                                }
                            });
                        }
                    }
                }
                "pyproject.toml" => {
                    let mut pep = |v: &Item| {
                        for dep in v
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(toml_edit::Value::as_str)
                        {
                            requirements(dep, &mut add);
                        }
                    };
                    pep(item(doc, &["project", "dependencies"]));
                    toml_entries(item(doc, &["project", "optional-dependencies"]), |_, v| {
                        pep(v)
                    });
                    toml_entries(item(doc, &["dependency-groups"]), |_, v| pep(v));
                    let poetry = item(doc, &["tool", "poetry"]);
                    let mut table = |v: &Item| {
                        toml_entries(v, |n, v| {
                            if n != "python"
                                && v.get("path").is_none()
                                && v.get("git").is_none()
                                && v.get("url").is_none()
                            {
                                add(
                                    n,
                                    v.as_str()
                                        .or_else(|| v.get("version").and_then(Item::as_str)),
                                );
                            }
                        })
                    };
                    table(item(poetry, &["dependencies"]));
                    table(item(poetry, &["dev-dependencies"]));
                    toml_entries(item(poetry, &["group"]), |_, v| {
                        table(item(v, &["dependencies"]))
                    });
                }
                _ => {
                    for p in item(doc, &["package"])
                        .as_array_of_tables()
                        .into_iter()
                        .flatten()
                    {
                        let source = p.get("source");
                        if file == "Cargo.lock"
                            && !source
                                .and_then(Item::as_str)
                                .is_some_and(|s| s.starts_with("registry+"))
                            || file != "Cargo.lock"
                                && source.is_some_and(|s| {
                                    ["directory", "editable", "virtual", "git", "url", "path"]
                                        .iter()
                                        .any(|k| s.get(k).is_some())
                                        || s.get("type").and_then(Item::as_str).is_some_and(|t| {
                                            matches!(t, "directory" | "file" | "git" | "url")
                                        })
                                })
                        {
                            continue;
                        }
                        if let Some(name) = p.get("name").and_then(Item::as_str) {
                            add(name, p.get("version").and_then(Item::as_str));
                        }
                    }
                }
            }
        }
        "pnpm-lock.yaml" | "pubspec.yaml" | "pubspec.lock" => {
            let Ok(v) = serde_yaml::from_str::<serde_yaml::Value>(text) else {
                return;
            };
            match file {
                "pnpm-lock.yaml" => {
                    if let Some(packages) = v["packages"].as_mapping() {
                        for id in packages.keys().filter_map(serde_yaml::Value::as_str) {
                            let id = id.trim_start_matches('/').split('(').next().unwrap_or("");
                            if let Some((name, version)) = id
                                .rsplit_once('@')
                                .filter(|(name, _)| !name.is_empty())
                                .or_else(|| id.rsplit_once('/'))
                            {
                                add(name, Some(version));
                            }
                        }
                    }
                }
                "pubspec.lock" => {
                    if let Some(packages) = v["packages"].as_mapping() {
                        for (n, p) in packages {
                            if p["source"].as_str() == Some("hosted") {
                                if let Some(n) = n.as_str() {
                                    add(n, p["version"].as_str());
                                }
                            }
                        }
                    }
                }
                _ => {
                    for key in ["dependencies", "dev_dependencies"] {
                        if let Some(deps) = v[key].as_mapping() {
                            for (n, p) in deps {
                                if let Some(n) = n.as_str() {
                                    if p.is_string() || p["version"].is_string() {
                                        add(n, p.as_str().or_else(|| p["version"].as_str()));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        "go.mod" => go(text, &mut add),
        "Gemfile.lock" => {
            let mut gem = false;
            for line in text.lines() {
                if !line.starts_with(' ') && !line.is_empty() {
                    gem = line == "GEM";
                }
                if gem && line.starts_with("    ") && !line.starts_with("     ") {
                    if let Some((name, version)) = line.trim().split_once(" (") {
                        add(name, version.strip_suffix(')'));
                    }
                }
            }
        }
        "yarn.lock" => yarn(text, &mut add),
        "bun.lock" => bun(text, &mut add),
        "build.gradle" | "build.gradle.kts" | "gradle.lockfile" => gradle(text, &mut add),
        "pom.xml" => maven(text, &mut add),
        "Package.swift" => swift(text, &mut add),
        _ if file.ends_with(".csproj") || file == "Directory.Packages.props" => {
            dotnet(text, &mut add)
        }
        _ => {
            for line in text.lines() {
                requirements(line, &mut add);
            }
        }
    }
}

fn go(text: &str, add: &mut impl FnMut(&str, Option<&str>)) {
    let mut block = "";
    let mut deps = BTreeMap::new();
    let mut replacements = BTreeMap::new();
    for line in text.lines() {
        let line = line.split("//").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line == ")" {
            block = "";
            continue;
        }
        let mut words = line.split_whitespace();
        let directive = if block.is_empty() {
            words.next().unwrap_or("")
        } else {
            block
        };
        let Some(name) = words.next() else { continue };
        if name == "(" {
            block = directive;
            continue;
        }
        match directive {
            "require" => {
                deps.insert(name, words.next());
            }
            "replace" => {
                let rest = words.collect::<Vec<_>>();
                if let Some(pos) = rest.iter().position(|&s| s == "=>") {
                    replacements.insert(
                        (name, (pos == 1).then(|| rest[0])),
                        (rest.get(pos + 1).copied(), rest.get(pos + 2).copied()),
                    );
                }
            }
            _ => {}
        }
    }
    for (name, version) in deps {
        match replacements
            .get(&(name, version))
            .or_else(|| replacements.get(&(name, None)))
        {
            Some((Some(replacement), Some(version))) => add(replacement, Some(version)),
            Some(_) => {}
            None => add(name, version),
        }
    }
}

fn yarn(text: &str, add: &mut impl FnMut(&str, Option<&str>)) {
    let mut key = "";
    for line in text.lines() {
        if !line.starts_with([' ', '#']) && line.ends_with(':') {
            key = line.trim_end_matches(':');
        }
        let line = line.trim();
        if let Some(version) = line
            .strip_prefix("version ")
            .or_else(|| line.strip_prefix("version:"))
        {
            for selector in key.trim_matches('"').split(", ") {
                let selector = selector.trim_matches('"');
                if let Some(i) = selector[selector.starts_with('@') as usize..].find('@') {
                    let i = i + selector.starts_with('@') as usize;
                    let (name, spec) = selector.split_at(i);
                    if !spec.starts_with("@workspace:") {
                        add(name, Some(version.trim().trim_matches('"')));
                    }
                }
            }
        }
    }
}

fn bun(text: &str, add: &mut impl FnMut(&str, Option<&str>)) {
    // bun.lock is JSON with comments and trailing commas. Package records start with a resolved ID.
    let re = version_regex(r#"(?m)^\s*"[^"]+":\s*\["(@?[^"@]+)@([^"@]+)""#);
    for p in re.captures_iter(text) {
        add(&p[1], Some(&p[2]));
    }
}

fn gradle(text: &str, add: &mut impl FnMut(&str, Option<&str>)) {
    let re = version_regex(r#"(?:["']|(?m:^))([\w.\-]+:[\w.\-]+):([^\s"'=]+)(?:["']|=)"#);
    for p in re.captures_iter(text) {
        add(&p[1], Some(&p[2]));
    }
}

fn maven(text: &str, add: &mut impl FnMut(&str, Option<&str>)) {
    let deps = version_regex(r"(?s)<dependency\s*>(.*?)</dependency\s*>");
    let field = version_regex(r"<([\w.\-]+)>\s*([^<>]+?)\s*</[\w.\-]+>");
    let props = version_regex(r"(?s)<properties>(.*?)</properties>");
    let props: BTreeMap<_, _> = props
        .captures(text)
        .and_then(|p| p.get(1))
        .into_iter()
        .flat_map(|p| field.captures_iter(p.as_str()))
        .filter_map(|p| Some((p.get(1)?.as_str(), p.get(2)?.as_str())))
        .collect();
    for dep in deps.captures_iter(text) {
        let Some(body) = dep.get(1) else { continue };
        let row: BTreeMap<_, _> = field
            .captures_iter(body.as_str())
            .filter_map(|p| Some((p.get(1)?.as_str(), p.get(2)?.as_str())))
            .collect();
        if let (Some(group), Some(artifact)) = (row.get("groupId"), row.get("artifactId")) {
            let version = row.get("version").and_then(|v| {
                if let Some(key) = v.strip_prefix("${").and_then(|v| v.strip_suffix('}')) {
                    props.get(key).copied()
                } else {
                    Some(*v)
                }
            });
            add(&format!("{group}:{artifact}"), version);
        }
    }
}

fn swift_name(url: &str) -> Option<&str> {
    let url = url.strip_prefix("https://github.com/")?;
    let name = url.trim_end_matches('/').trim_end_matches(".git");
    (name.split('/').count() == 2 && package_name(name)).then_some(name)
}

fn swift(text: &str, add: &mut impl FnMut(&str, Option<&str>)) {
    let re = version_regex(
        r#"(?s)\.package\s*\(\s*(?:name:\s*"[^"]*"\s*,\s*)?url:\s*"([^"]+)"\s*,\s*(?:exact:\s*"([^"]+)"|from:|\.upToNext|\.exact\(\s*"([^"]+)"|"[^"]+"\s*\.\.)"#,
    );
    for p in re.captures_iter(text) {
        if let Some(name) = swift_name(&p[1]) {
            add(name, p.get(2).or_else(|| p.get(3)).map(|v| v.as_str()));
        }
    }
}

fn dotnet(text: &str, add: &mut impl FnMut(&str, Option<&str>)) {
    let tags = version_regex(
        r"(?s)<Package(?:Reference|Version)\b([^>]+?)(?:/\s*>|>(.*?)</Package(?:Reference|Version)\s*>)",
    );
    let attr = version_regex(r#"\b(Include|Update|Version)\s*=\s*["']([^"']+)["']"#);
    let version = version_regex(r"<Version>\s*([^<]+?)\s*</Version>");
    for tag in tags.captures_iter(text) {
        let attrs: BTreeMap<_, _> = attr
            .captures_iter(tag.get(1).map_or("", |v| v.as_str()))
            .filter_map(|p| Some((p.get(1)?.as_str(), p.get(2)?.as_str())))
            .collect();
        if let Some(name) = attrs.get("Include").or_else(|| attrs.get("Update")) {
            let v = attrs.get("Version").copied().or_else(|| {
                version
                    .captures(tag.get(2)?.as_str())?
                    .get(1)
                    .map(|v| v.as_str())
            });
            add(
                name,
                v.map(|s| s.trim_start_matches('[').trim_end_matches(']')),
            );
        }
    }
}
