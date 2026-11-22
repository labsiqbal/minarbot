//! Adapted from colbymchenry/codegraph v1.6.2 (6560052), src/resolution/frameworks/
//! express.ts, nestjs.ts, nextjs.ts, python.ts, react.ts, react-router.ts and
//! src/resolution/{next-router,react-router}-synthesizer.ts.
//! License: MIT (Copyright (c) 2026 Colby Mchenry).

use super::{dirname, imports::Module, js, Match, Reference, Resolver};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet},
    sync::LazyLock,
};

macro_rules! re {
    ($pattern:literal) => {{
        static RE: LazyLock<regex::Regex> = LazyLock::new(|| crate::literal_regex($pattern));
        &*RE
    }};
}

impl Resolver<'_> {
    pub(super) fn framework_match(&self, reference: &Reference, module: &Module) -> Option<Match> {
        let name = reference.name.as_str();
        let file = reference.file.as_str();
        let language = reference.language.as_str();
        let (target, confidence, framework) = if language == "python" {
            if self.config.django
                || module
                    .imports
                    .iter()
                    .any(|i| i.source.starts_with("django"))
            {
                let convention = if name.ends_with("View") || name.ends_with("ViewSet") {
                    "views"
                } else if name.ends_with("Form") {
                    "forms"
                } else if name.ends_with("Model") || re!(r"^[A-Z][a-z]+$").is_match(name) {
                    "models"
                } else if name == "_iterable_class" {
                    let target = self.named("__iter__").iter().copied().find(|&id| {
                        let node = &self.symbols[id];
                        node.file.ends_with("/django/db/models/query.py")
                            && node.owner() == Some("ModelIterable")
                    })?;
                    return Some(framework_hit(target, 0.7, "django"));
                } else {
                    ""
                };
                let target = self.named(name).iter().copied().find(|&id| {
                    let n = &self.symbols[id];
                    n.language == "python"
                        && n.kind == "class"
                        && !convention.is_empty()
                        && n.file.split('/').any(|part| {
                            part == convention || part.strip_suffix(".py") == Some(convention)
                        })
                });
                if let Some(target) = target {
                    return Some(framework_hit(target, 0.8, "django"));
                }
            }
            if !(name.ends_with("_bp") || name.ends_with("_blueprint")) || !self.config.flask {
                return None;
            }
            let target = self.named(name).iter().copied().find(|&id| {
                let n = &self.symbols[id];
                n.language == "python" && matches!(n.kind.as_str(), "variable" | "constant")
            })?;
            (target, 0.8, "flask")
        } else if js(language) {
            if module.imports.iter().any(|imp| imp.local == name) {
                return None;
            }
            if let Some(suffix) = [
                "Service",
                "Controller",
                "Guard",
                "Interceptor",
                "Pipe",
                "Filter",
                "Gateway",
                "Resolver",
                "Module",
            ]
            .into_iter()
            .find(|suffix| name.ends_with(suffix))
            .filter(|_| {
                self.config
                    .depends_on(file, |name| name.starts_with("@nestjs/"))
                    || module.text.contains("@nestjs/")
            }) {
                let convention = format!(".{}.", suffix.to_ascii_lowercase());
                if let Some(target) = self.named(name).iter().copied().find(|&id| {
                    let node = &self.symbols[id];
                    node.kind == "class"
                        && js(&node.language)
                        && (node.file == file
                            || dirname(&node.file) == dirname(file)
                                && node
                                    .file
                                    .rsplit('/')
                                    .next()
                                    .is_some_and(|f| f.contains(&convention)))
                }) {
                    return Some(framework_hit(target, 0.85, "nestjs"));
                }
            }
            if self.config.express {
                if let Some((owner, method)) = name.split_once('.') {
                    if let Some(suffix) = ["Controller", "Service", "Helper", "Utils", "Util"]
                        .into_iter()
                        .find(|suffix| owner.ends_with(suffix))
                    {
                        let target = self.named(method).iter().copied().find(|&id| {
                            let n = &self.symbols[id];
                            n.file == file && n.kind == "method" && n.owner() == Some(owner)
                        });
                        if let Some(target) = target {
                            return Some(framework_hit(
                                target,
                                if suffix == "Controller" { 0.85 } else { 0.8 },
                                "express",
                            ));
                        }
                    }
                }
            }
            let jsx = matches!(language, "jsx" | "tsx");
            if !jsx
                && !self
                    .config
                    .depends_on(file, |pkg| matches!(pkg, "react" | "react-native" | "next"))
            {
                return None;
            }
            let hook = name.starts_with("use") && name.len() > 3;
            let component = jsx && name.starts_with(|c: char| c.is_ascii_uppercase());
            let context = name.ends_with("Context") || name.ends_with("Provider");
            let target = self.named(name).iter().copied().find(|&id| {
                let n = &self.symbols[id];
                n.file == file
                    && n.id != reference.source
                    && ((hook && n.kind == "function" && n.owner().is_none())
                        || (component
                            && matches!(n.kind.as_str(), "component" | "function" | "class"))
                        || (context
                            && matches!(n.kind.as_str(), "variable" | "constant" | "function")))
            })?;
            (target, if hook { 0.85 } else { 0.8 }, "react")
        } else {
            return None;
        };
        Some(framework_hit(target, confidence, framework))
    }
}

fn framework_hit(target: usize, confidence: f64, framework: &'static str) -> Match {
    let mut hit = Match::new(target, confidence, "framework");
    hit.framework = Some(framework);
    hit
}

struct Route {
    id: String,
    name: String,
    qualified: String,
    file: String,
    language: String,
    line: u32,
    width: u32,
    framework: &'static str,
    handler: Option<(String, &'static str, u32)>,
    inline: Vec<String>,
    exported: bool,
    mount: Option<String>,
    app: Option<String>,
}

impl Route {
    fn new(
        file: &str,
        language: &str,
        span: (u32, u32),
        verb: &str,
        path: &str,
        framework: &'static str,
    ) -> Self {
        let (line, width) = span;
        let page = verb.is_empty();
        let key = if page {
            format!("route:{path}")
        } else {
            format!("{verb}:{path}")
        };
        let id = if page && framework == "nextjs" {
            format!("route:{file}:{path}")
        } else if page {
            format!("route:{file}:{line}:{path}")
        } else {
            format!("route:{file}:{line}:{verb}:{path}")
        };
        Self {
            id,
            name: if page {
                path.to_owned()
            } else {
                format!("{verb} {path}")
            },
            qualified: format!("{file}::{key}"),
            file: file.to_owned(),
            language: language.to_owned(),
            line,
            width,
            framework,
            handler: None,
            inline: Vec::new(),
            exported: framework == "nextjs",
            mount: None,
            app: None,
        }
    }

    fn target(&mut self, name: &str, kind: &'static str, line: u32) {
        if !name.is_empty() {
            self.handler = Some((name.to_owned(), kind, line));
        }
    }
}

fn top_level_args(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut stack = Vec::new();
    let mut quote = None;
    let mut escape = false;
    let mut start = 0;
    for (i, b) in text.bytes().take(64 * 1024).enumerate() {
        if let Some(q) = quote {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == q {
                quote = None;
            }
            continue;
        }
        match b {
            b'"' | b'\'' | 96 => quote = Some(b),
            b'(' => stack.push(b')'),
            b'{' => stack.push(b'}'),
            b'[' => stack.push(b']'),
            b')' | b'}' | b']' if stack.last() == Some(&b) => {
                stack.pop();
            }
            b')' if stack.is_empty() => {
                out.push(text[start..i].trim());
                return out;
            }
            b',' if stack.is_empty() => {
                out.push(text[start..i].trim());
                start = i + 1;
            }
            _ => (),
        }
    }
    Vec::new()
}

fn literal(text: &str) -> Option<&str> {
    let text = text.trim();
    let quote = text.chars().next()?;
    if !matches!(quote, '\'' | '"' | '\x60') {
        return None;
    }
    let body = text.get(1..)?;
    let end = body.find(quote)?;
    Some(&body[..end])
}

fn blank_comments<'a>(source: &'a str, language: &str) -> std::borrow::Cow<'a, str> {
    let python = language == "python";
    let lex = if python {
        re!(
            r#"(?s)"""(?:\\.|.)*?"""|'''(?:\\.|.)*?'''|"(?:(?:\\.)|[^"\\\n])*"|'(?:(?:\\.)|[^'\\\n])*'|(\#[^\r\n]*)"#
        )
    } else {
        re!(
            r#"(?s)"(?:\\.|[^"\\\n])*"|'(?:\\.|[^'\\\n])*'|\x60(?:\\.|[^\x60\\])*\x60|(//[^\r\n]*|/\*.*?(?:\*/|$))"#
        )
    };
    lex.replace_all(source, |cap: &regex::Captures| {
        if let Some(comment) = cap.get(1) {
            comment
                .as_str()
                .bytes()
                .map(|b| {
                    if matches!(b, b'\r' | b'\n') {
                        char::from(b)
                    } else {
                        ' '
                    }
                })
                .collect()
        } else {
            cap[0].to_owned()
        }
    })
}

fn line_at(module: &Module, byte: usize) -> u32 {
    module.lines.partition_point(|&start| start <= byte) as u32
}

fn join_http(prefix: &str, path: &str) -> String {
    let prefix = prefix.trim_matches('/');
    let path = path.trim_matches('/');
    match (prefix.is_empty(), path.is_empty()) {
        (true, _) => format!("/{path}"),
        (_, true) => format!("/{prefix}"),
        _ => format!("/{prefix}/{path}"),
    }
}

fn express(file: &str, lang: &str, module: &Module, safe: &str) -> Vec<Route> {
    let route_call = re!(
        r#"\b(?:app|router)\s*\.\s*(get|post|put|patch|delete|all|use)\s*\(\s*['"]([^'"]+)['"]\s*,|\b(?:app|router)\s*\.\s*route\s*\(\s*['"]([^'"]+)['"]\s*\)\s*\.\s*(get|post|put|patch|delete|all)\s*\("#
    );
    let mut routes = Vec::new();
    for cap in route_call.captures_iter(safe) {
        let Some(full) = cap.get(0) else {
            continue;
        };
        let balanced = if let (Some(verb), Some(path)) = (cap.get(1), cap.get(2)) {
            Some((verb.as_str(), path.as_str()))
        } else {
            cap.get(4)
                .zip(cap.get(3))
                .map(|(verb, path)| (verb.as_str(), path.as_str()))
        };
        let Some((verb, path)) = balanced else {
            continue;
        };
        if verb == "use" && !path.starts_with('/') {
            continue;
        }
        let line = line_at(module, full.start());
        let mut route = Route::new(
            file,
            lang,
            (line, full.as_str().encode_utf16().count() as u32),
            &verb.to_ascii_uppercase(),
            path,
            "express",
        );
        let args = top_level_args(&safe[full.end()..]);
        if let Some(handler) = args.last().copied() {
            let plain = re!(r"^[A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)*$");
            if plain.is_match(handler) {
                route.target(handler, "references", line);
                if verb == "use" {
                    route.mount = Some(handler.to_owned());
                }
            } else if handler.contains("=>") || handler.starts_with("function") {
                let body = handler.split_once('{').map_or(handler, |(_, body)| body);
                let mut seen = HashSet::new();
                for cap in re!(r"\b([A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)*)\s*\(")
                    .captures_iter(body)
                    .take(128)
                {
                    let Some(name) = cap.get(1).map(|m| m.as_str()) else {
                        continue;
                    };
                    if !matches!(
                        name,
                        "if" | "for" | "while" | "switch" | "function" | "catch"
                    ) && !name.starts_with("res.")
                        && seen.insert(name)
                    {
                        route.inline.push(name.to_owned());
                    }
                }
            }
        }
        routes.push(route);
    }
    routes
}

fn python(file: &str, module: &Module, safe: &str, django: bool, flask: bool) -> Vec<Route> {
    let mut routes = Vec::new();
    if django {
        let urlpatterns = re!(
            r#"\b(?:path|re_path|url)\s*\(\s*r?['"]([^'"]*)['"]\s*,\s*([\w.]+(?:\s*\([^)]*\))?)"#
        );
        for cap in urlpatterns.captures_iter(safe) {
            let Some(full) = cap.get(0) else {
                continue;
            };
            let line = line_at(module, full.start());
            let mut route = Route::new(
                file,
                "python",
                (line, full.as_str().encode_utf16().count() as u32),
                "",
                &cap[1],
                "django",
            );
            let handler = &cap[2];
            if let Some(module) = handler
                .strip_prefix("include")
                .and_then(|s| s.trim_start().strip_prefix('('))
                .and_then(literal)
            {
                route.target(module, "imports", line);
            } else {
                route.target(
                    handler
                        .trim_end_matches(".as_view()")
                        .split('(')
                        .next()
                        .unwrap_or(handler)
                        .trim_end_matches(".as_view")
                        .trim(),
                    "references",
                    line,
                );
            }
            routes.push(route);
        }
        for cap in re!(r#"\.register\s*\(\s*r?['"]([^'"]+)['"]\s*,\s*([\w.]+View(?:Set)?)\b"#)
            .captures_iter(safe)
        {
            let Some(full) = cap.get(0) else {
                continue;
            };
            let line = line_at(module, full.start());
            let mut route = Route::new(
                file,
                "python",
                (line, full.as_str().encode_utf16().count() as u32),
                "VIEWSET",
                &join_http("", &cap[1]),
                "django",
            );
            route.target(&cap[2], "references", line);
            routes.push(route);
        }
    }
    if flask {
        for cap in re!(r#"@[\w.]+\.(route|get|post|put|patch|delete)\s*\(\s*['"]([^'"]*)['"]"#)
            .captures_iter(safe)
        {
            let Some(full) = cap.get(0) else {
                continue;
            };
            let Some(open) = full.as_str().find('(') else {
                continue;
            };
            let args = top_level_args(&safe[full.start() + open + 1..]);
            if args.is_empty() {
                continue;
            }
            let line = line_at(module, full.start());
            let handler = re!(r"(?m)^[ \t]*(?:async[ \t]+)?def[ \t]+(\w+)")
                .captures(&safe[full.end()..])
                .and_then(|cap| cap.get(1));
            let default = if &cap[1] == "route" {
                "GET".to_owned()
            } else {
                cap[1].to_ascii_uppercase()
            };
            let methods = flask_methods(&args, &default);
            for method in methods {
                let mut route = Route::new(
                    file,
                    "python",
                    (line, full.as_str().encode_utf16().count() as u32),
                    &method,
                    if cap[2].is_empty() { "/" } else { &cap[2] },
                    "flask",
                );
                if let Some(handler) = handler {
                    route.target(handler.as_str(), "references", line);
                }
                routes.push(route);
            }
        }
        for cap in re!(r"\.(add_url_rule|add\w*[Rr]esource)\s*\(").captures_iter(safe) {
            let Some(full) = cap.get(0) else {
                continue;
            };
            let line = line_at(module, full.start());
            let args = top_level_args(&safe[full.end()..]);
            let Some(first) = args.first() else {
                continue;
            };
            let resource = &cap[1] != "add_url_rule";
            let (paths, handler) = if resource {
                (
                    args.iter()
                        .skip(1)
                        .filter_map(|arg| literal(arg))
                        .collect::<Vec<_>>(),
                    *first,
                )
            } else {
                let path = literal(first.strip_prefix("rule=").unwrap_or(first))
                    .into_iter()
                    .collect();
                let view = args
                    .iter()
                    .find_map(|arg| {
                        let (name, value) = arg.split_once('=')?;
                        (name.trim() == "view_func").then_some(value.trim())
                    })
                    .or_else(|| args.get(2).copied())
                    .unwrap_or_default();
                (path, view)
            };
            let class_view = resource || handler.contains(".as_view");
            let target = handler
                .split('(')
                .next()
                .unwrap_or(handler)
                .trim_end_matches(".as_view")
                .trim();
            let methods = flask_methods(&args, if class_view { "ANY" } else { "GET" });
            for path in paths.into_iter().filter(|path| path.starts_with('/')) {
                for method in &methods {
                    let mut route = Route::new(file, "python", (line, 0), method, path, "flask");
                    route.target(target, "references", line);
                    routes.push(route);
                }
            }
        }
    }
    routes
}

fn flask_methods(args: &[&str], default: &str) -> Vec<String> {
    let methods = args.iter().find_map(|arg| {
        let (name, methods) = arg.split_once('=')?;
        (name.trim() == "methods").then_some(methods)
    });
    match methods {
        Some(methods) => re!(r#"['"]([A-Za-z]+)['"]"#)
            .captures_iter(methods)
            .map(|cap| cap[1].to_ascii_uppercase())
            .collect(),
        None => vec![default.to_owned()],
    }
}

fn nest(file: &str, lang: &str, module: &Module, safe: &str, prefix: &str) -> Vec<Route> {
    let mut routes = Vec::new();
    let classes = re!(r"@(Controller|Resolver|WebSocketGateway|Injectable|Module)\s*\(");
    let decorators = re!(
        r"@(Get|Post|Put|Patch|Delete|Head|Options|All|Query|Mutation|Subscription|MessagePattern|EventPattern|SubscribeMessage)\s*\("
    );
    let mut scopes = Vec::new();
    for cap in classes.captures_iter(safe) {
        let Some(full) = cap.get(0) else {
            continue;
        };
        let args = top_level_args(&safe[full.end()..]);
        let arg = args.first().copied().unwrap_or_default();
        let path = literal(arg)
            .or_else(|| {
                re!(r"\b(?:path|namespace)\s*:\s*")
                    .find(arg)
                    .and_then(|m| literal(&arg[m.end()..]))
            })
            .unwrap_or_default();
        scopes.push((
            full.start(),
            &safe[full.start() + 1..full.start() + cap[1].len() + 1],
            path,
        ));
    }
    for cap in decorators.captures_iter(safe) {
        let Some(full) = cap.get(0) else {
            continue;
        };
        let line = line_at(module, full.start());
        let args = top_level_args(&safe[full.end()..]);
        let arc = scopes
            .iter()
            .rev()
            .find(|(start, _, _)| *start < full.start());
        let (_, scope, scope_path) = arc.copied().unwrap_or((0, "", ""));
        let ident = &cap[1];
        if matches!(ident, "Query" | "Mutation" | "Subscription") && scope != "Resolver" {
            continue;
        }
        let method = match ident {
            "MessagePattern" => "MESSAGE".to_owned(),
            "EventPattern" => "EVENT".to_owned(),
            "SubscribeMessage" => "WS".to_owned(),
            _ => ident.to_ascii_uppercase(),
        };
        let arg = args.first().copied().unwrap_or_default();
        let handler = re!(r"(?m)(?:^|[)\n])[ \t]*(?:(?:public|private|protected|static|async|override)[ \t]+)*([A-Za-z_$][\w$]*)[ \t]*\(")
            .captures(&safe[full.end()..safe.floor_char_boundary((full.end() + 2048).min(safe.len()))])
            .and_then(|c| c.get(1)).map(|m| m.as_str());
        let path = if matches!(
            ident,
            "Get" | "Post" | "Put" | "Patch" | "Delete" | "Head" | "Options" | "All"
        ) {
            join_http(
                prefix,
                &join_http(scope_path, literal(arg).unwrap_or_default()),
            )
        } else if ident == "SubscribeMessage" && !scope_path.is_empty() {
            join_http(scope_path, literal(arg).or(handler).unwrap_or_default())
        } else {
            literal(arg).or(handler).unwrap_or_default().to_owned()
        };
        let mut route = Route::new(
            file,
            lang,
            (
                line,
                full.as_str().encode_utf16().count() as u32
                    + args
                        .iter()
                        .map(|arg| arg.encode_utf16().count() as u32)
                        .sum::<u32>()
                    + 1,
            ),
            &method,
            &path,
            "nestjs",
        );
        if let Some(handler) = handler {
            route.target(handler, "references", line);
        }
        routes.push(route);
    }
    routes
}

fn next(file: &str, lang: &str, module: &Module, safe: &str) -> Vec<Route> {
    if file
        .split('/')
        .any(|seg| matches!(seg, "__tests__" | "__mocks__" | "node_modules"))
    {
        return Vec::new();
    }
    let file_pattern = re!(r"^(.*?)(?:src/)?(app|pages)/(.+)\.(?:tsx|ts|jsx|js|mjs|cjs)$");
    let Some(cap) = file_pattern.captures(file) else {
        return Vec::new();
    };
    let root = &cap[1];
    if !root.is_empty() && !root.ends_with('/') {
        return Vec::new();
    }
    let page = &cap[2] == "pages";
    let mut segments = cap[3].split('/').collect::<Vec<_>>();
    let Some(leaf) = segments.pop() else {
        return Vec::new();
    };
    if page {
        if leaf.starts_with('_')
            || [".test", ".spec", ".stories", ".config", ".d"]
                .iter()
                .any(|suffix| leaf.ends_with(suffix))
        {
            return Vec::new();
        }
        if leaf != "index" {
            segments.push(leaf);
        }
    } else if !matches!(leaf, "page" | "route") {
        return Vec::new();
    }
    if !page {
        if segments
            .iter()
            .any(|s| s.starts_with('@') || s.starts_with("(."))
        {
            return Vec::new();
        }
        segments.retain(|s| !(s.starts_with('(') && s.ends_with(')')));
    }
    let path = format!(
        "/{}",
        segments
            .iter()
            .map(|segment| {
                if let Some(s) = segment
                    .strip_prefix("[[...")
                    .and_then(|s| s.strip_suffix("]]"))
                    .or_else(|| {
                        segment
                            .strip_prefix("[...")
                            .and_then(|s| s.strip_suffix(']'))
                    })
                {
                    format!(":{s}*")
                } else if let Some(s) = segment.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
                {
                    format!(":{s}")
                } else {
                    (*segment).to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("/")
    );
    if !page && leaf == "route" {
        let mut methods = HashSet::new();
        return re!(r"\bexport\s+(?:(?:async\s+)?function|const|let)\s+(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS)\b")
            .captures_iter(safe).filter_map(|m| {
                let full = m.get(0)?;
                let method = m.get(1)?.as_str();
                if !methods.insert(method) { return None; }
                let line = line_at(module, full.start());
                let mut route = Route::new(file, lang, (line, full.len() as u32), method, &path, "nextjs");
                route.target(method, "references", line);
                Some(route)
            }).collect();
    }
    let api = page && segments.first() == Some(&"api");
    let mut route = Route::new(
        file,
        lang,
        (1, 0),
        if api { "ANY" } else { "" },
        &path,
        "nextjs",
    );
    if !api {
        route.app = Some(root.to_owned());
    }
    if let Some(cap) =
        re!(r"\bexport\s+default\s+(?:(?:async\s+)?function\s+|class\s+)?([A-Za-z_$][\w$]*)\b")
            .captures(safe)
    {
        if let (Some(full), Some(name)) = (cap.get(0), cap.get(1)) {
            if !matches!(name.as_str(), "function" | "class" | "async") {
                route.target(
                    name.as_str(),
                    if api { "references" } else { "calls" },
                    line_at(module, full.start()),
                );
            }
        }
    }
    vec![route]
}

fn react(file: &str, lang: &str, module: &Module, safe: &str) -> Vec<Route> {
    let mut routes = Vec::new();
    for cap in re!(r#"<Route\b[^>]*?\bpath\s*=\s*['"]([^'"]*)['"]"#).captures_iter(safe) {
        let Some(full) = cap.get(0) else {
            continue;
        };
        let line = line_at(module, full.start());
        let mut route = Route::new(file, lang, (line, 0), "", &join_http("", &cap[1]), "react");
        let tail = &safe[full.end()..];
        if let Some(handler) =
            re!(r"^[^>]*?\b(?:element|Component|component)\s*=\s*\{\s*<?([A-Z]\w*)")
                .captures(tail)
                .and_then(|m| m.get(1))
        {
            route.target(handler.as_str(), "references", line);
        }
        route.app = Some(String::new());
        routes.push(route);
    }
    if safe.contains("createBrowserRouter")
        || safe.contains("createHashRouter")
        || safe.contains("createMemoryRouter")
    {
        for cap in re!(r#"\{\s*path\s*:\s*['"]([^'"]*)['"]\s*,\s*(?:element\s*:\s*<|Component\s*:\s*)([A-Z]\w*)"#).captures_iter(safe) {
            let Some(full) = cap.get(0) else { continue; };
            let line = line_at(module, full.start());
            let mut route = Route::new(file, lang, (line, 0), "", &join_http("", &cap[1]), "react");
            route.target(&cap[2], "references", line);
            route.app = Some(String::new());
            routes.push(route);
        }
    }
    routes
}

pub(super) fn refresh(db: &Connection, resolver: &mut Resolver<'_>) -> rusqlite::Result<()> {
    let mut files = resolver
        .files
        .keys()
        .filter(|file| {
            resolver.file_node(file).is_some_and(|id| {
                let lang = &resolver.symbols[id].language;
                js(lang) || lang == "python"
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    files.sort();
    let mut routes = Vec::new();
    let mut prefixes = BTreeMap::<String, String>::new();
    for file in files.iter().filter(|file| {
        file.rsplit('/')
            .next()
            .is_some_and(|f| matches!(f, "main.ts" | "main.js"))
    }) {
        let module = resolver.module(file, "typescript");
        if module.text.contains("NestFactory") {
            if let Some(value) =
                re!(r#"\bsetGlobalPrefix\s*\(\s*['"]([^'"]+)['"]"#).captures(&module.text)
            {
                prefixes.insert(dirname(file).to_owned(), value[1].to_owned());
            }
        }
    }
    for file in &files {
        let Some(index) = resolver.file_node(file) else {
            continue;
        };
        let lang = resolver.symbols[index].language.clone();
        let module = resolver.module(file, &lang);
        let source = &module.text;
        if lang == "python" {
            let django = resolver.config.django
                || source.contains("django.urls")
                || source.contains("rest_framework");
            let flask =
                resolver.config.flask || module.imports.iter().any(|imp| imp.source == "flask");
            if django || flask {
                routes.extend(python(
                    file,
                    &module,
                    &blank_comments(source, &lang),
                    django,
                    flask,
                ));
            }
            continue;
        }
        let express_app = resolver.config.express
            || source.contains("express(")
            || source.contains("express.Router(")
            || (file.contains("routes/") || file.contains("controllers/"))
                && (source.contains("app.get") || source.contains("router.get"));
        let nest_app = resolver
            .config
            .depends_on(file, |name| name.starts_with("@nestjs/"))
            || source.contains("@Controller(")
            || source.contains("@Resolver(")
            || source.contains("@WebSocketGateway(");
        let next_app = resolver.config.depends_on(file, |name| name == "next")
            || ["next.config.js", "next.config.mjs", "next.config.ts"]
                .iter()
                .any(|name| !resolver.in_file(name).is_empty());
        let react_app = source.contains("<Route")
            || source.contains("createBrowserRouter")
            || source.contains("createHashRouter")
            || source.contains("createMemoryRouter");
        if !(express_app || nest_app || next_app || react_app) {
            continue;
        }
        let safe = blank_comments(source, &lang);
        if express_app {
            routes.extend(express(file, &lang, &module, &safe));
        }
        if nest_app {
            let prefix = prefixes
                .iter()
                .filter(|(root, _)| {
                    root.is_empty()
                        || file
                            .strip_prefix(root.as_str())
                            .is_some_and(|rest| rest.starts_with('/'))
                })
                .max_by_key(|(root, _)| root.len())
                .map_or("", |(_, path)| path.as_str());
            routes.extend(nest(file, &lang, &module, &safe, prefix));
        }
        if next_app {
            routes.extend(next(file, &lang, &module, &safe));
        }
        if react_app {
            routes.extend(react(file, &lang, &module, &safe));
        }
    }

    let mut mounts = BTreeMap::<String, Vec<String>>::new();
    for route in &routes {
        if let Some(handler) = &route.mount {
            let module = resolver.module(&route.file, &route.language);
            if let Some(path) = module
                .imports
                .iter()
                .find(|imp| imp.local == *handler)
                .and_then(|imp| resolver.import_path(&imp.source, &route.file, &route.language))
            {
                mounts
                    .entry(path)
                    .or_default()
                    .push(route.name.trim_start_matches("USE ").to_owned());
            }
        }
    }
    for route in &mut routes {
        if route.framework == "express" && route.mount.is_none() {
            if let Some(prefixes) = mounts.get(&route.file) {
                if let [prefix] = prefixes.as_slice() {
                    if let Some((verb, path)) = route.name.split_once(' ') {
                        route.name = format!("{verb} {}", join_http(prefix, path));
                    }
                }
            }
        }
    }

    let now = crate::store::timestamp(std::time::SystemTime::now());
    let mut node = db.prepare_cached("INSERT OR IGNORE INTO nodes
        (id,kind,name,qualified_name,file_path,language,start_line,end_line,start_column,end_column,is_exported,updated_at)
        VALUES (?1,'route',?2,?3,?4,?5,?6,?6,0,?7,?8,?9)")?;
    let mut edge = db.prepare_cached(
        "INSERT OR IGNORE INTO edges
        (source,target,kind,line,col,metadata,provenance) VALUES (?1,?2,?3,?4,0,?5,?6)",
    )?;
    let mut route_ids = HashSet::new();
    for route in &routes {
        if !route_ids.insert(&route.id) {
            continue;
        }
        node.execute(params![
            route.id,
            route.name,
            route.qualified,
            route.file,
            route.language,
            route.line,
            route.width,
            route.exported,
            now
        ])?;
        let module = resolver.module(&route.file, &route.language);
        if let Some((handler, kind, line)) = &route.handler {
            if let Some(target) = route_target(resolver, &module, route, handler, kind, *line) {
                let metadata = json!({"resolvedBy":"framework","confidence":0.9,"framework":route.framework,"refName":handler});
                edge.execute(params![
                    route.id,
                    resolver.symbols[target].id,
                    kind,
                    line,
                    metadata.to_string(),
                    Option::<&str>::None
                ])?;
            }
        }
        for name in &route.inline {
            if let Some(target) = route_target(resolver, &module, route, name, "calls", route.line)
            {
                let metadata = json!({"synthesizedBy":"express-handler","via":name});
                edge.execute(params![
                    route.id,
                    resolver.symbols[target].id,
                    "calls",
                    route.line,
                    metadata.to_string(),
                    "heuristic"
                ])?;
            }
        }
    }
    navigation(resolver, &routes, &files, &mut edge)
}

fn route_target(
    resolver: &mut Resolver<'_>,
    module: &Module,
    route: &Route,
    name: &str,
    kind: &str,
    line: u32,
) -> Option<usize> {
    if kind == "imports" {
        return resolver
            .import_path(name, &route.file, &route.language)
            .and_then(|path| resolver.file_node(&path));
    }
    let reference = Reference {
        row: 0,
        source: route.id.clone(),
        name: name.to_owned(),
        kind: kind.to_owned(),
        line,
        column: 0,
        file: route.file.clone(),
        language: route.language.clone(),
        candidates: None,
    };
    if let Some(hit) = resolver.via_import(&reference, module) {
        return Some(hit.target);
    }
    if module
        .imports
        .iter()
        .any(|imp| imp.local == name.split('.').next().unwrap_or(name))
    {
        return None;
    }
    let name = super::tail(name);
    resolver
        .named(name)
        .iter()
        .copied()
        .filter(|&index| {
            let n = &resolver.symbols[index];
            n.file == route.file
                && matches!(
                    n.kind.as_str(),
                    "function" | "method" | "class" | "component" | "variable" | "constant"
                )
        })
        .min_by_key(|&index| resolver.symbols[index].start.abs_diff(line))
}

fn navigation(
    resolver: &mut Resolver<'_>,
    routes: &[Route],
    files: &[String],
    edge: &mut rusqlite::Statement<'_>,
) -> rusqlite::Result<()> {
    let pages = routes
        .iter()
        .filter(|route| route.app.is_some())
        .collect::<Vec<_>>();
    if pages.is_empty() {
        return Ok(());
    }
    let calls = re!(
        r"\b(router\.(?:push|replace)|NextResponse\.(?:redirect|rewrite)|redirect|permanentRedirect|navigate)\s*\("
    );
    let links = re!(r"<(Link|NavLink|Navigate|a)\b[^>]*?\b(?:href|to)\s*=\s*\{?\s*");
    for file in files {
        let Some(file_id) = resolver.file_node(file) else {
            continue;
        };
        let lang = resolver.symbols[file_id].language.clone();
        if !js(&lang) {
            continue;
        }
        let module = resolver.module(file, &lang);
        let safe = blank_comments(&module.text, &lang);
        for (regex, link) in [(calls, true), (links, false)] {
            for cap in regex.captures_iter(&safe) {
                let Some(full) = cap.get(0) else {
                    continue;
                };
                let line = line_at(&module, full.start());
                let verb = &cap[1];
                let next = module.text.contains("next/navigation")
                    || module.text.contains("next/router")
                    || module.text.contains("next/link")
                    || module.text.contains("next/server")
                    || resolver.config.depends_on(file, |pkg| pkg == "next");
                let react = module.text.contains("react-router")
                    || resolver
                        .config
                        .depends_on(file, |pkg| pkg.starts_with("react-router"));
                let Some(href) = literal(&safe[full.end()..]).or_else(|| {
                    let tail = safe[full.end()..].trim_start();
                    literal(
                        tail.strip_prefix("new URL")?
                            .trim_start()
                            .strip_prefix('(')?,
                    )
                }) else {
                    continue;
                };
                if !href.starts_with('/') || href.starts_with("//") {
                    continue;
                }
                let path = href.split(['?', '#']).next().unwrap_or(href);
                let destination = pages
                    .iter()
                    .copied()
                    .filter(|page| {
                        page.app.as_ref().is_some_and(|root| file.starts_with(root))
                            && if page.framework == "nextjs" {
                                next && verb != "navigate"
                            } else {
                                react
                                    && !verb.starts_with("router.")
                                    && !verb.starts_with("NextResponse.")
                            }
                            && path_matches(&page.name, path)
                    })
                    .max_by_key(|page| {
                        (
                            page.app.as_ref().map_or(0, String::len),
                            page.name == path,
                            page.name
                                .split('/')
                                .filter(|seg| !seg.starts_with(':'))
                                .count(),
                        )
                    });
                let Some(target) = destination else {
                    continue;
                };
                let origin = resolver
                    .in_file(file)
                    .iter()
                    .copied()
                    .filter(|&id| {
                        let n = &resolver.symbols[id];
                        matches!(n.kind.as_str(), "function" | "method" | "component")
                            && n.start <= line
                            && line <= n.end
                    })
                    .max_by_key(|&id| resolver.symbols[id].start)
                    .unwrap_or(file_id);
                let source = &resolver.symbols[origin].id;
                let mut metadata: Value =
                    json!({"href":href,"navMethod":verb.rsplit('.').next().unwrap_or(verb)});
                if link {
                    metadata["resolvedBy"] = json!("framework");
                    metadata["confidence"] = json!(0.95);
                } else {
                    metadata["synthesizedBy"] = json!(if target.framework == "nextjs" {
                        "next-link"
                    } else {
                        "react-router-link"
                    });
                    metadata["navMethod"] = json!(if verb == "a" { "a" } else { "link" });
                    metadata["registeredAt"] = json!(format!("{file}:{line}"));
                }
                edge.execute(params![
                    source,
                    target.id,
                    "navigates",
                    line,
                    metadata.to_string(),
                    "heuristic"
                ])?;
            }
        }
    }
    Ok(())
}

fn path_matches(pattern: &str, path: &str) -> bool {
    let mut path = path.split('/').filter(|s| !s.is_empty());
    for segment in pattern.split('/').filter(|s| !s.is_empty()) {
        if segment.starts_with(':') && segment.ends_with('*') {
            return true;
        }
        match path.next() {
            Some(part) if segment.starts_with(':') || segment == part => (),
            _ => return false,
        }
    }
    path.next().is_none()
}
