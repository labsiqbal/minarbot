//! Adapted from colbymchenry/codegraph v1.6.2 (6560052), src/resolution/
//! name-matcher.ts, index.ts, and type-parameters.ts.
//! License: MIT (Copyright (c) 2026 Colby Mchenry).

use super::{
    builtins::*, dirname, family, imports::Module, inheritance, js, member, tail, type_kind, Match,
    Reference, Resolver, Symbol,
};
use std::{collections::HashSet, sync::LazyLock};

macro_rules! re {
    ($pattern:literal) => {{
        static RE: LazyLock<regex::Regex> = LazyLock::new(|| crate::literal_regex($pattern));
        &*RE
    }};
}

impl Resolver<'_> {
    pub(super) fn resolve_reference(&mut self, reference: &Reference) -> Option<Match> {
        let name = reference.name.as_str();
        let lang = reference.language.as_str();
        let is_js = js(lang);
        let call = reference.kind == "calls";
        let importing = reference.kind == "imports";
        let part = tail(name);
        if self.named(name).is_empty()
            && self.named(part).is_empty()
            && !is_js
            && lang != "python"
            && !name.contains("::")
            && !importing
        {
            return None;
        }
        let module = self.module(&reference.file, lang);
        if self.builtin(reference, &module) {
            return None;
        }
        let site = module.at(reference.line, reference.column);
        let receiver = receiver_of(name, site, call);
        let chain_receiver = (receiver.is_none() && call && matches!(lang, "rust" | "go"))
            .then(|| rust_go_receiver(reference, &module))
            .flatten();
        let receiver = receiver.or(chain_receiver.as_deref());
        if reference.kind == "function_ref" {
            return self.function_ref(reference, &module, receiver);
        }
        if is_js && call && receiver.is_none() {
            if let Some(hit) = self.destructured_call(reference, &module) {
                return Some(hit);
            }
        }
        let root = name.split('.').next().unwrap_or(name);
        let chain = name.contains("().");
        let opaque_js =
            is_js && call && (chain || name.matches('.').count() > 1 && !name.starts_with("this."));
        let namespace = module
            .imports
            .iter()
            .any(|imp| imp.name == "*" && imp.local == root);
        let import = if (opaque_js && (!namespace || chain))
            || (lang == "python" && call && matches!(receiver, Some("self" | "cls")))
            || (lang == "python" && chain)
        {
            None
        } else {
            self.via_import(reference, &module)
                .and_then(|hit| self.target_kind(reference, hit))
        };
        if opaque_js || lang == "python" && chain {
            return import;
        }
        if import.is_some_and(|hit| hit.confidence >= 0.9) {
            return import;
        }
        if is_js && self.external_binding(reference, &module, root) {
            return self.express_middleware(reference);
        }
        if lang == "go" && reference.kind == "references" && !name.contains('.') {
            let line = module.line(reference.line);
            let prefix = &line[..line.len() - site.len()];
            let receiver = re!(r"(\w+)\.\s*$").captures(prefix);
            let qualified = receiver.as_ref().map(|m| &m[1]).or_else(|| {
                site.split_once('.')
                    .filter(|(_, at)| at.starts_with(name))
                    .and_then(|(before, _)| re!(r"\w+$").find(before).map(|m| m.as_str()))
            });
            if qualified.is_some_and(|pkg| {
                module
                    .imports
                    .iter()
                    .any(|imp| imp.local == pkg && self.go_package(&imp.source).is_none())
            }) {
                return None;
            }
        }
        if lang == "go"
            && !importing
            && name.contains('.')
            && module
                .imports
                .iter()
                .any(|imp| imp.local == root && self.go_package(&imp.source).is_none())
        {
            return None;
        }
        if lang == "rust" && name.contains("::") {
            let root = name.trim_start_matches("::").split("::").next()?;
            if self.external_rust(root)
                || module.rust_uses.iter().any(|imp| {
                    imp.local == root
                        && self.external_rust(&imp.source)
                        && self
                            .named(root)
                            .iter()
                            .all(|&id| !type_kind(&self.symbols[id].kind))
                })
            {
                return None;
            }
        }
        let framework = if lang == "rust" {
            self.rust_framework(reference, &module)
        } else if matches!(lang, "javascript" | "typescript") {
            self.express_middleware(reference)
        } else {
            None
        };
        let qualified = self.qualified_match(reference);

        let mut result = qualified.or_else(|| self.receiver_match(reference, &module, receiver));
        if result.is_none() {
            result = self.exact(reference, &module, receiver);
        }
        let result = result
            .and_then(|hit| self.target_kind(reference, hit))
            .filter(|hit| self.visible(hit.target, reference))
            .filter(|hit| {
                self.symbols[hit.target].id != reference.source
                    || !call
                    || name.contains(['.', ':']) && !matches!(receiver, Some("super" | "super()"))
                    || (receiver.is_none()
                        || matches!(receiver, Some("self" | "Self" | "this" | "cls")))
            });
        [framework, import, result]
            .into_iter()
            .flatten()
            .reduce(|best, next| {
                if next.confidence > best.confidence {
                    next
                } else {
                    best
                }
            })
    }

    fn destructured_call(&mut self, reference: &Reference, module: &Module) -> Option<Match> {
        let scope = self.enclosing_callable(reference);
        let before = module.range(scope.map_or(1, |scope| scope.start), reference.line);
        let pattern = re!(
            r"\b(?:const|let|var)\s*\{([^{}]*)\}\s*=\s*(?:await\s+)?([A-Za-z_$][\w$]*)\s*(?:<[^<>()]*>)?\s*\("
        );
        let binding = pattern
            .captures_iter(before)
            .filter_map(|capture| {
                let list = capture.get(1)?.as_str();
                let callee = capture.get(2)?.as_str();
                let key = list.split(',').find_map(|part| {
                    let part = part.split('=').next()?.trim();
                    let (binding, local) = part
                        .split_once(':')
                        .map_or((part, part), |(binding, local)| {
                            (binding.trim(), local.trim())
                        });
                    (local == reference.name && re!(r"^[A-Za-z_$][\w$]*$").is_match(binding))
                        .then_some(binding)
                })?;
                Some((key, callee))
            })
            .last()?;
        let (key, callee) = binding;
        let mut callee_ref = reference.clone();
        callee_ref.name = callee.to_owned();
        let holder = self
            .via_import(&callee_ref, module)
            .map(|hit| hit.target)
            .or_else(|| {
                let candidates = self
                    .named(callee)
                    .iter()
                    .copied()
                    .filter(|&id| {
                        let n = &self.symbols[id];
                        js(&n.language)
                            && matches!(n.kind.as_str(), "function" | "constant" | "variable")
                            && self.lexical(n, reference)
                    })
                    .collect::<Vec<_>>();
                candidates
                    .iter()
                    .copied()
                    .find(|&id| self.symbols[id].file == reference.file)
                    .or_else(|| {
                        candidates
                            .first()
                            .copied()
                            .filter(|_| candidates.len() == 1)
                    })
            })?;
        let n = &self.symbols[holder];
        let (file, lang, start, end) = (n.file.clone(), n.language.clone(), n.start, n.end);
        let holder_module = self.module(&file, &lang);
        let returns = re!(r"\breturn\s*\{([^{}]*)\}")
            .captures_iter(holder_module.range(start, end))
            .any(|body| body[1].split(',').any(|field| field.trim() == key));
        if !returns {
            return None;
        }
        let hit = self
            .named(key)
            .iter()
            .copied()
            .filter(|&id| {
                let n = &self.symbols[id];
                id != holder
                    && n.file == file
                    && callable(&n.kind)
                    && (n.owner().is_none() || n.start >= start && n.end <= end)
            })
            .min_by_key(|&id| {
                let n = &self.symbols[id];
                (n.start < start || n.end > end, n.kind != "function")
            })?;
        Some(Match::new(hit, 0.85, "instance-method"))
    }

    fn target_kind(&self, reference: &Reference, mut hit: Match) -> Option<Match> {
        let n = &self.symbols[hit.target];
        if !family(&reference.language, &n.language)
            || reference.kind == "imports" && member(&n.kind)
        {
            return None;
        }
        if inheritance(&reference.kind) && n.id == reference.source {
            return None;
        }
        if reference.kind == "calls"
            && n.id == reference.source
            && matches!(n.kind.as_str(), "constant" | "variable")
        {
            return None;
        }
        if inheritance(&reference.kind) && !type_kind(&n.kind) {
            if !js(&n.language) || !matches!(n.kind.as_str(), "constant" | "variable") {
                return None;
            }
            hit.target = self.named(&n.name).iter().copied().find(|&id| {
                let candidate = &self.symbols[id];
                candidate.file == n.file && type_kind(&candidate.kind)
            })?;
        }
        Some(hit)
    }

    fn builtin(&self, reference: &Reference, module: &Module) -> bool {
        let name = reference.name.as_str();
        if js(&reference.language) {
            return (JS_BUILT_INS.contains(&name)
                && !module.imports.iter().any(|imp| imp.local == name))
                || REACT_HOOKS.contains(&name)
                || ["console.", "Math.", "JSON."]
                    .iter()
                    .any(|prefix| name.starts_with(prefix));
        }
        if reference.language == "go" {
            return GO_BUILT_INS.contains(&name)
                || name
                    .split_once('.')
                    .is_some_and(|(package, _)| GO_STDLIB_PACKAGES.contains(&package));
        }
        if reference.language == "python" {
            if PYTHON_BUILT_INS.contains(&name) {
                return true;
            }
            if let Some((receiver, method)) = name.split_once('.') {
                return PYTHON_BUILT_IN_TYPES.contains(&receiver)
                    || (PYTHON_BUILT_IN_METHODS.contains(&method)
                        && !self
                            .named(&capitalize(receiver))
                            .iter()
                            .any(|&id| type_kind(&self.symbols[id].kind))
                        && !module.imports.iter().any(|imp| {
                            if imp.local != receiver {
                                return false;
                            }
                            let source = if imp.name == "*" {
                                imp.source.clone()
                            } else {
                                format!(
                                    "{}{}{}",
                                    imp.source,
                                    if imp.source.ends_with('.') { "" } else { "." },
                                    imp.name
                                )
                            };
                            self.import_path(&source, &reference.file, "python")
                                .is_some()
                        }));
            }
        }
        false
    }

    fn external_binding(&self, reference: &Reference, module: &Module, name: &str) -> bool {
        module.imports.iter().any(|imp| {
            imp.local == name
                && self.config.external_package(&reference.file, &imp.source)
                && self
                    .import_path(&imp.source, &reference.file, &reference.language)
                    .is_none()
        })
    }

    fn qualified_match(&self, reference: &Reference) -> Option<Match> {
        let name = reference.name.as_str();
        if !name.contains(['.', ':']) {
            return None;
        }
        let suitable = |id: usize| {
            let n = &self.symbols[id];
            family(&n.language, &reference.language)
                && (n.kind != "import" || reference.kind == "imports" && n.file == reference.file)
        };
        let exact = self
            .qualified(name)
            .iter()
            .copied()
            .filter(|&id| suitable(id))
            .collect::<Vec<_>>();
        if let [id] = exact.as_slice() {
            return Some(Match::new(*id, 0.95, "qualified-name"));
        }
        if let Some(id) = exact
            .iter()
            .copied()
            .find(|&id| self.symbols[id].file == reference.file)
        {
            return Some(Match::new(id, 0.95, "qualified-name"));
        }
        self.named(tail(name))
            .iter()
            .copied()
            .filter(|&id| suitable(id) && self.symbols[id].qualified.ends_with(name))
            .min_by_key(|&id| self.symbols[id].file != reference.file)
            .map(|id| Match::new(id, 0.85, "qualified-name"))
    }

    fn exact(
        &mut self,
        reference: &Reference,
        module: &Module,
        receiver: Option<&str>,
    ) -> Option<Match> {
        let call = reference.kind == "calls";
        let importing = reference.kind == "imports";
        let lang = reference.language.as_str();
        let name = reference.name.as_str();
        let bare = call
            && receiver.is_none()
            && module
                .at(reference.line, reference.column)
                .strip_prefix(name)
                .is_some_and(|rest| rest.trim_start().starts_with(['(', '!', ':']));
        let current_binding = bare
            && js(lang)
            && self.named(name).iter().any(|&id| {
                let n = &self.symbols[id];
                n.file == reference.file
                    && !member(&n.kind)
                    && n.kind != "import"
                    && self.lexical(n, reference)
            });
        let scope = bare.then(|| self.enclosing_callable(reference)).flatten();
        let local =
            bare && (js(lang) || lang == "python") && locally_bound(name, reference, module, scope);
        if lang == "python"
            && bare
            && module.imports.iter().any(|imp| {
                imp.local == name
                    && self
                        .import_path(&imp.source, &reference.file, "python")
                        .is_none()
            })
        {
            return None;
        }
        let mut candidates = Vec::new();
        for &id in self.named(name) {
            let n = &self.symbols[id];
            if n.kind == "import"
                || !family(lang, &n.language)
                || (inheritance(&reference.kind) && !type_kind(&n.kind))
                || (importing && member(&n.kind))
                || (bare && member(&n.kind))
                || (lang == "rust" && !self.rust_in_scope(n, reference, module))
                || !self.lexical(n, reference)
            {
                continue;
            }
            if call && lang == "python" {
                if let Some(receiver) = receiver
                    .filter(|receiver| !matches!(*receiver, "self" | "cls" | "super" | "super()"))
                {
                    let root = receiver.rsplit('.').next().unwrap_or(receiver);
                    if n.kind == "method" {
                        if n.owner()
                            .is_none_or(|owner| plain(tail(owner)) != plain(root))
                        {
                            continue;
                        }
                    } else if module_name(&n.file) != root.trim_end_matches("()") {
                        continue;
                    }
                }
            }
            if let Some(receiver) = receiver.filter(|_| call && matches!(lang, "rust" | "go")) {
                if n.kind == "function"
                    || (n.kind == "method"
                        && std_method(lang, name)
                        && !matches!(receiver, "Self" | "self")
                        && !overlap(receiver, n))
                {
                    continue;
                }
            }
            if current_binding && n.file != reference.file {
                continue;
            }
            if local
                && (n.file != reference.file
                    || scope.is_none_or(|scope| n.start < scope.start || n.start > scope.end))
            {
                continue;
            }
            candidates.push(id);
            if candidates.len() > self.ceiling {
                return None;
            }
        }
        let id = best(self, reference, &candidates)?;
        let n = &self.symbols[id];
        let confidence = if candidates.len() == 1 {
            if n.language == lang {
                0.9
            } else {
                0.5
            }
        } else if proximity(&n.file, &reference.file) >= 30 {
            0.7
        } else {
            0.4
        };
        self.visible(id, reference)
            .then(|| Match::new(id, confidence, "exact-match"))
    }

    fn function_ref(
        &mut self,
        reference: &Reference,
        module: &Module,
        receiver: Option<&str>,
    ) -> Option<Match> {
        let name = reference.name.as_str();
        let lang = reference.language.as_str();
        if let Some(method) = name.strip_prefix("this.") {
            let owner = self.owner_at(reference)?;
            let local = self
                .qualified(&format!("{owner}::{method}"))
                .iter()
                .copied()
                .filter(|&id| {
                    let n = &self.symbols[id];
                    n.file == reference.file && n.id != reference.source && callable(&n.kind)
                })
                .min_by_key(|&id| self.symbols[id].start);
            if let Some(target) = local {
                return Some(Match::new(target, 0.95, "function-ref"));
            }
            let target = self.method_on_type(&owner, method, reference, true)?;
            return Some(Match::new(target, 0.9, "function-ref"));
        }
        let imported = self.via_import(reference, module).filter(|hit| {
            let n = &self.symbols[hit.target];
            callable(&n.kind) || lang == "python" && n.kind == "class"
        });
        if let Some(receiver) =
            receiver.filter(|_| matches!(lang, "python" | "go") && name.contains('.'))
        {
            let target = if let Some(hit) = imported {
                hit.target
            } else {
                let owner = self.receiver_type(reference, module, receiver)?;
                self.method_on_type(&owner, tail(name), reference, false)?
            };
            if self.python_property(target) {
                return None;
            }
            return (self.symbols[target].id != reference.source)
                .then(|| Match::new(target, 0.9, "function-ref"));
        }
        if imported.is_some() {
            return imported;
        }
        let explicit = name.contains("::");
        let pool = self
            .named(tail(name))
            .iter()
            .copied()
            .filter(|&id| {
                let n = &self.symbols[id];
                n.id != reference.source
                    && family(lang, &n.language)
                    && (n.kind == "function"
                        || (explicit || !js(lang) && lang != "python") && n.kind == "method"
                        || lang == "python" && n.kind == "class")
                    && (!explicit
                        || n.qualified == name
                        || n.qualified
                            .strip_suffix(name)
                            .is_some_and(|prefix| prefix.ends_with("::")))
                    && self.lexical(n, reference)
            })
            .collect::<Vec<_>>();
        let scope = self.enclosing_callable(reference);
        let bound = (js(lang) || lang == "python") && locally_bound(name, reference, module, scope);
        let same = pool
            .iter()
            .copied()
            .filter(|&id| {
                let n = &self.symbols[id];
                n.file == reference.file
                    && (!bound
                        || scope.is_some_and(|scope| {
                            lang != "python" && n.start >= scope.start && n.start <= scope.end
                        }))
            })
            .collect::<Vec<_>>();
        if let Some(id) = same
            .iter()
            .copied()
            .min_by_key(|&id| self.symbols[id].start)
        {
            let confidence = if same.len() == 1 && !explicit {
                0.95
            } else {
                0.9
            };
            return Some(Match::new(id, confidence, "function-ref"));
        }
        if let [id] = pool.as_slice() {
            if !bound && self.visible(*id, reference) {
                return Some(Match::new(
                    *id,
                    if explicit { 0.9 } else { 0.8 },
                    "function-ref",
                ));
            }
        }
        None
    }

    fn python_property(&mut self, id: usize) -> bool {
        let node = &self.symbols[id];
        if node.language != "python" || node.kind != "method" {
            return false;
        }
        let (file, start) = (node.file.clone(), node.start);
        let module = self.module(&file, "python");
        (1..start)
            .rev()
            .map(|line| module.line(line).trim())
            .take_while(|line| line.starts_with('@'))
            .any(|line| {
                matches!(
                    line,
                    "@property" | "@cached_property" | "@functools.cached_property"
                )
            })
    }

    fn receiver_match(
        &mut self,
        reference: &Reference,
        module: &Module,
        receiver: Option<&str>,
    ) -> Option<Match> {
        let lang = reference.language.as_str();
        let name = reference.name.as_str();
        let method = tail(name);
        let receiver = receiver?;
        let chained = name.contains("().");
        if lang == "python" && matches!(receiver, "self" | "cls") && !name.contains('.') {
            return None;
        }
        if js(lang) && matches!(receiver, "this" | "super") && !name.contains('.') {
            if let Some(owner) = self.owner_at(reference) {
                if let Some(target) = self.method_on_type(&owner, method, reference, true) {
                    return Some(Match::new(target, 0.9, "instance-method"));
                }
            }
            return None;
        }
        let inferred = if let Some((factory, _)) = name.rsplit_once("().") {
            self.named(tail(factory))
                .iter()
                .map(|&id| &self.symbols[id])
                .find(|n| {
                    family(lang, &n.language)
                        && callable(&n.kind)
                        && (!factory.contains("::") || n.qualified.ends_with(factory))
                })
                .and_then(|n| {
                    let ty = n
                        .return_type
                        .as_deref()
                        .or_else(|| n.signature.as_deref()?.rsplit_once("->").map(|(_, ty)| ty))?;
                    if ty.trim() == "Self" {
                        n.owner().map(str::to_owned)
                    } else {
                        simple_type(ty).map(str::to_owned)
                    }
                })
        } else {
            self.receiver_type(reference, module, receiver).or_else(|| {
                (lang == "python" && reference.kind == "calls" && name.contains('.'))
                    .then(|| self.python_fixture_type(reference, module, receiver))
                    .flatten()
            })
        };
        if let Some(owner) = inferred {
            let self_call = lang == "rust" && matches!(receiver, "self" | "Self");
            if let Some(id) = self.method_on_type(
                &owner,
                method,
                reference,
                matches!(receiver, "super" | "super()"),
            ) {
                return Some(Match::new(
                    id,
                    if chained
                        || receiver.contains('.') && (lang == "go" || lang == "rust" || js(lang))
                    {
                        0.85
                    } else {
                        0.9
                    },
                    if self_call {
                        "qualified-name"
                    } else {
                        "instance-method"
                    },
                ));
            }
            if js(lang)
                && (JS_BUILT_INS.contains(&owner.as_str())
                    || TS_PRIMITIVE_TYPES.contains(&owner.as_str()))
                || matches!(receiver, "this" | "self" | "super" | "super()" | "cls")
                || lang != "rust"
                    && owner.starts_with(char::is_uppercase)
                    && !self.named(&owner).iter().any(|&id| {
                        let n = &self.symbols[id];
                        type_kind(&n.kind) && family(&n.language, lang)
                    })
            {
                return None;
            }
        }
        if lang == "rust" && name.contains("::")
            || receiver.starts_with(char::is_uppercase) && self.named(receiver).is_empty()
            || lang == "go" && receiver.contains('.')
        {
            return None;
        }
        if !name.contains(['.', ':'])
            || receiver.contains("()")
            || receiver.starts_with("this.")
            || receiver.starts_with("self.")
            || js(lang) && JS_BUILTIN_METHODS.contains(&method)
        {
            return None;
        }
        let capitalized = capitalize(receiver);
        if self
            .named(&capitalized)
            .iter()
            .any(|&id| type_kind(&self.symbols[id].kind))
        {
            if let Some(id) = self.method_on_type(&capitalized, method, reference, false) {
                let direct = receiver == capitalized;
                let owns = self.symbols[id]
                    .owner()
                    .is_some_and(|owner| tail(owner) == capitalized);
                return Some(Match::new(
                    id,
                    if direct && owns { 0.85 } else { 0.8 },
                    if direct {
                        "qualified-name"
                    } else {
                        "instance-method"
                    },
                ));
            }
        }
        if self.named(method).len() > self.ceiling {
            return None;
        }
        let mut same = self
            .named(method)
            .iter()
            .copied()
            .filter(|&id| {
                let n = &self.symbols[id];
                n.kind == "method"
                    && n.language == lang
                    && (!test_path(&n.file) || test_path(&reference.file))
            })
            .collect::<Vec<_>>();
        if same.len() > self.ceiling {
            return None;
        }
        same.sort_by_key(|&id| self.symbols[id].file != reference.file);
        let unique = same.len() == 1;
        let target = same.into_iter().find(|&id| {
            let n = &self.symbols[id];
            (unique || n.id != reference.source)
                && (overlap(receiver, n)
                    || unique
                        && !std_method(lang, method)
                        && (!js(lang) || !JS_BUILTIN_METHODS.contains(&method)))
        })?;
        Some(Match::new(
            target,
            if unique { 0.7 } else { 0.65 },
            "instance-method",
        ))
    }

    pub(super) fn method_on_type(
        &self,
        owner: &str,
        method: &str,
        reference: &Reference,
        parents_only: bool,
    ) -> Option<usize> {
        self.walk_method(
            owner,
            method,
            reference,
            parents_only,
            &mut HashSet::new(),
            0,
        )
    }

    fn walk_method(
        &self,
        owner: &str,
        method: &str,
        reference: &Reference,
        parents_only: bool,
        seen: &mut HashSet<String>,
        depth: usize,
    ) -> Option<usize> {
        if depth > 4 || !seen.insert(owner.to_owned()) {
            return None;
        }
        if !parents_only {
            let wanted = format!("{owner}::{method}");
            let matches = self
                .named(method)
                .iter()
                .copied()
                .filter(|&id| {
                    let n = &self.symbols[id];
                    n.kind == "method"
                        && family(&n.language, &reference.language)
                        && (n.qualified == wanted
                            || n.qualified
                                .strip_suffix(&wanted)
                                .is_some_and(|prefix| prefix.ends_with("::")))
                })
                .collect::<Vec<_>>();
            if reference.kind == "function_ref" && matches.len() > 1 {
                return None;
            }
            if let Some(id) = matches
                .into_iter()
                .min_by_key(|&id| self.symbols[id].file != reference.file)
            {
                return Some(id);
            }
        }
        let mut declarations = self
            .named(tail(owner))
            .iter()
            .copied()
            .filter(|&id| {
                let n = &self.symbols[id];
                type_kind(&n.kind) && family(&n.language, &reference.language)
            })
            .collect::<Vec<_>>();
        declarations.sort_by_key(|&id| self.symbols[id].file != reference.file);
        for declaration in declarations {
            if let Some(parents) = self.supers.get(&declaration) {
                for &parent in parents {
                    if let Some(id) = self.walk_method(
                        &self.symbols[parent].qualified,
                        method,
                        reference,
                        false,
                        seen,
                        depth + 1,
                    ) {
                        return Some(id);
                    }
                }
            }
        }
        None
    }

    fn owner_at(&self, reference: &Reference) -> Option<String> {
        if let Some(owner) = self
            .origin(reference)
            .filter(|n| n.kind == "method")
            .and_then(Symbol::owner)
        {
            return Some(owner.to_owned());
        }
        self.in_file(&reference.file)
            .iter()
            .map(|&i| &self.symbols[i])
            .filter(|n| type_kind(&n.kind) && n.start <= reference.line && reference.line <= n.end)
            .min_by_key(|n| n.end - n.start)
            .map(|n| n.qualified.clone())
    }

    fn receiver_type(
        &mut self,
        reference: &Reference,
        module: &Module,
        receiver: &str,
    ) -> Option<String> {
        if matches!(
            receiver,
            "self" | "Self" | "this" | "cls" | "super" | "super()"
        ) {
            return self.owner_at(reference);
        }
        if let Some(field) = receiver
            .strip_prefix("self.")
            .or_else(|| receiver.strip_prefix("this."))
        {
            let owner = self.owner_at(reference)?;
            let n = self
                .named(tail(&owner))
                .iter()
                .map(|&id| &self.symbols[id])
                .filter(|n| type_kind(&n.kind) && n.file == reference.file)
                .min_by_key(|n| n.start.abs_diff(reference.line))?;
            if reference.language == "rust" {
                let pattern =
                    crate::literal_regex(&format!(r"\b{}\s*:\s*([^,{{}}]+)", regex::escape(field)));
                let text = module.range(n.start, n.end);
                let raw = pattern.captures(text)?.get(1)?.as_str();
                return rust_field_type(raw).map(str::to_owned);
            }
            return infer_binding(module, field, n.start, n.end, &reference.language);
        }
        let scope = self.enclosing_callable(reference);
        let start = scope.map_or(1, |n| n.start);
        let lang = &reference.language;
        let local = infer_binding(module, receiver, start, reference.line, lang)
            .or_else(|| infer_on_line(scope?.signature.as_deref()?, receiver, lang));
        if local.is_some() {
            return local;
        }
        if lang == "go" {
            let (base, field) = receiver.split_once('.')?;
            let owner = infer_binding(module, base, start, reference.line, lang)?;
            let n = self
                .named(&owner)
                .iter()
                .map(|&id| &self.symbols[id])
                .find(|n| n.kind == "struct" && dirname(&n.file) == dirname(&reference.file))?;
            let (file, start, end) = (n.file.clone(), n.start, n.end);
            return infer_binding(&self.module(&file, lang), field, start, end, lang);
        }
        None
    }

    fn enclosing_callable(&self, reference: &Reference) -> Option<&Symbol> {
        if let Some(scope) = self.origin(reference).filter(|n| callable(&n.kind)) {
            return Some(scope);
        }
        self.in_file(&reference.file)
            .iter()
            .map(|&id| &self.symbols[id])
            .filter(|n| {
                callable(&n.kind)
                    && (n.start, n.column) <= (reference.line, reference.column)
                    && (reference.line, reference.column) <= (n.end, n.end_column)
            })
            .min_by_key(|n| n.end - n.start)
    }

    // pytest injects only fixtures visible to the caller. Prefer the fixture
    // defined in the test module, then the nearest conftest declaration.
    fn python_fixture_type(
        &mut self,
        reference: &Reference,
        module: &Module,
        receiver: &str,
    ) -> Option<String> {
        if !re!(r"^[a-z_]\w*$").is_match(receiver) || matches!(receiver, "self" | "cls") {
            return None;
        }
        let caller = self.origin(reference).filter(|n| callable(&n.kind))?;
        let signature = module.range(caller.start, caller.start.saturating_add(4));
        let signature = re!(r"\)\s*(?:->[^:]*)?:")
            .find(signature)
            .map_or(signature, |end| &signature[..end.start()]);
        if !signature
            .split(['(', ','])
            .skip(1)
            .any(|part| part.trim_start().split([':', '=', ' ', ')']).next() == Some(receiver))
        {
            return None;
        }
        let caller_dir = std::path::Path::new(dirname(&reference.file));
        let candidates = self.named(receiver).to_vec();
        let mut fixtures = Vec::new();
        for id in candidates {
            let n = &self.symbols[id];
            if n.kind != "function" || n.language != "python" {
                continue;
            }
            let local = n.file == reference.file;
            let conftest = n.file.rsplit('/').next() == Some("conftest.py");
            if !(local || conftest && caller_dir.starts_with(dirname(&n.file))) {
                continue;
            }
            let (file, start, end) = (n.file.clone(), n.start, n.end);
            let fixture = self.module(&file, "python");
            if !conftest
                && !(start.saturating_sub(15).max(1)..start)
                    .rev()
                    .map(|line| fixture.line(line).trim())
                    .take_while(|line| {
                        !["def ", "async def ", "class "]
                            .iter()
                            .any(|prefix| line.starts_with(prefix))
                    })
                    .any(|line| re!(r"^@(?:\w+\.)*fixture\b").is_match(line))
            {
                continue;
            }
            fixtures.push((local, file.len(), fixture, start, end));
        }
        let (_, _, fixture, start, end) = fixtures
            .into_iter()
            .max_by_key(|(local, length, ..)| (*local, *length))?;
        let body = fixture.range(start.saturating_add(1), end);
        let returned =
            re!(r"(?m)^\s*(?:return|yield)\s+([A-Za-z_][\w.]*)\s*(\()?").captures(body)?;
        let mut name = returned.get(1)?.as_str();
        if returned.get(2).is_none() {
            name = re!(r"(?m)^\s*([A-Za-z_]\w*)\s*=\s*([A-Za-z_][\w.]*)\s*\(")
                .captures_iter(body)
                .find(|binding| &binding[1] == name)?
                .get(2)?
                .as_str();
        }
        let name = tail(name);
        name.starts_with(char::is_uppercase)
            .then(|| name.to_owned())
    }

    fn lexical(&self, n: &Symbol, reference: &Reference) -> bool {
        let scope = n.scope.get_or_init(|| {
            if matches!(n.kind.as_str(), "variable" | "constant") {
                return self
                    .in_file(&n.file)
                    .iter()
                    .map(|&id| &self.symbols[id])
                    .filter(|parent| {
                        callable(&parent.kind)
                            && parent.start < parent.end
                            && (parent.start, parent.column) < (n.start, n.column)
                            && n.start <= parent.end
                    })
                    .min_by_key(|parent| parent.end - parent.start)
                    .map(|parent| (parent.start, parent.end));
            }
            if !callable(&n.kind) && !type_kind(&n.kind) {
                return None;
            }
            let mut parent = n.owner()?;
            let first = parent;
            loop {
                let scope = self
                    .qualified(parent)
                    .iter()
                    .map(|&id| &self.symbols[id])
                    .find(|container| {
                        container.file == n.file
                            && callable(&container.kind)
                            && container.start <= n.start
                            && container.end >= n.end
                    });
                if let Some(container) = scope {
                    return (n.kind != "method" || parent != first)
                        .then_some((container.start, container.end));
                }
                parent = parent.rsplit_once("::")?.0;
            }
        });
        scope.is_none_or(|(start, end)| {
            n.file == reference.file && start <= reference.line && reference.line <= end
        })
    }

    fn express_middleware(&self, reference: &Reference) -> Option<Match> {
        if !matches!(reference.language.as_str(), "javascript" | "typescript") || !self.config.express || !re!(r"(?i)^(?:auth|authenticate|authorization|validate.*|sanitize.*|rateLimit.*|cors|helmet|logger|errorHandler|notFound|\w*Middleware)$").is_match(&reference.name) {
            return None;
        }
        let target = self.named(&reference.name).iter().copied().find(|&id| {
            let n = &self.symbols[id];
            n.file == reference.file
                && matches!(
                    n.kind.as_str(),
                    "function" | "method" | "variable" | "constant" | "class"
                )
        })?;
        let mut hit = Match::new(target, 0.8, "framework");
        hit.framework = Some("express");
        self.target_kind(reference, hit)
    }

    fn rust_framework(&mut self, reference: &Reference, module: &Module) -> Option<Match> {
        if !self.config.rust_manifest {
            return None;
        }
        let name = reference.name.as_str();
        let (kinds, dirs, score): (&[&str], &[&str], f64) =
            if name.ends_with("_handler") || name.starts_with("handle_") {
                (
                    &["function"],
                    &["handlers", "handler", "api", "routes", "controllers"],
                    0.8,
                )
            } else if name.ends_with("Service") || name.ends_with("Repository") {
                (
                    &["struct", "trait"],
                    &["services", "service", "repository", "domain"],
                    0.8,
                )
            } else if re!(r"^[A-Z][a-zA-Z]+$").is_match(name) {
                (
                    &["struct"],
                    &["models", "model", "entities", "entity", "domain", "types"],
                    0.7,
                )
            } else {
                return None;
            };
        let candidates = self.named(name).to_vec();
        let mut best = None;
        for id in candidates {
            let n = &self.symbols[id];
            if n.language != "rust"
                || !kinds.contains(&n.kind.as_str())
                || inheritance(&reference.kind) && n.id == reference.source
                || !self.lexical(n, reference)
                || !self.rust_in_scope(n, reference, module)
            {
                continue;
            }
            let rank = if n.file == reference.file {
                0
            } else if dirname(&n.file).split('/').any(|dir| dirs.contains(&dir)) {
                1
            } else {
                2
            };
            if best.is_none_or(|(best_rank, _)| rank < best_rank) && self.visible(id, reference) {
                best = Some((rank, id));
            }
        }
        let (_, id) = best?;
        let mut hit = Match::new(id, score, "framework");
        hit.framework = Some("rust");
        self.target_kind(reference, hit)
    }

    fn visible(&mut self, id: usize, reference: &Reference) -> bool {
        let node = &self.symbols[id];
        if node.file == reference.file {
            return true;
        }
        if test_path(&node.file) && !test_path(&reference.file) {
            return false;
        }
        if node.language == "go" {
            return node.name.starts_with(|c: char| c.is_ascii_uppercase())
                || dirname(&node.file) == dirname(&reference.file);
        }
        if node.language == "rust" {
            let dir = dirname(&node.file);
            return node.visibility.as_deref() != Some("private")
                || reference.file.starts_with(&format!("{dir}/"));
        }
        if js(&node.language)
            && (node.file.ends_with(".d.ts")
                || node.file.ends_with(".d.mts")
                || node.file.ends_with(".d.cts"))
        {
            return true;
        }
        if js(&node.language) {
            let (file, lang, start, name, exposed) = (
                node.file.clone(),
                node.language.clone(),
                node.start,
                node.name.clone(),
                node.exported || node.owner().is_some(),
            );
            let target = self.module(&file, &lang);
            return !target.sealed && (exposed || !target.esm ||
                target.export_local(&name) ||
                !re!(r"^\s*(?:declare\s+)?(?:async\s+)?(?:function\*?|const|let|var|(?:abstract\s+)?class|interface|type|enum)\s").is_match(target.line(start)));
        }
        true
    }

    fn rust_in_scope(&self, node: &Symbol, reference: &Reference, module: &Module) -> bool {
        let name = reference.name.as_str();
        if reference.kind == "references" && re!(r"^[A-Za-z_]\w*$").is_match(name) {
            let line = module.line(reference.line);
            let at = module.at(reference.line, reference.column);
            let before = &line[..line.len() - at.len()];
            let qualified = if at.starts_with(name) {
                re!(r"([\w:]+)::\s*$")
                    .captures(before)
                    .and_then(|m| m.get(1))
                    .map(|m| m.as_str())
            } else {
                re!(r"([\w:]+)::(\w+)")
                    .captures_iter(at)
                    .find(|m| &m[2] == name)
                    .and_then(|m| m.get(1))
                    .map(|m| m.as_str())
            };
            if let Some(path) = qualified {
                let root = path.split("::").next().unwrap_or_default();
                if root == "Self" {
                    return node.kind == "type_alias"
                        && node.file == reference.file
                        && rust_impl_line(module, node.start)
                            == rust_impl_line(module, reference.line);
                }
                if node.file != reference.file
                    && (self.external_rust(root)
                        || module
                            .rust_uses
                            .iter()
                            .any(|imp| imp.local == root && self.external_rust(&imp.source)))
                {
                    return false;
                }
                let segment = tail(path);
                if matches!(root, "crate" | "self" | "super")
                    || self.config.cargo_roots.contains(root)
                    || node.file == reference.file
                {
                    return true;
                }
                return module_name(&node.file) == segment
                    || dirname(&node.file).split('/').any(|part| part == segment)
                    || node.qualified.split("::").any(|part| part == segment);
            }
        }
        if node.kind == "enum_member" {
            return reference.kind != "references"
                && (node.owner().is_some_and(|owner| {
                    module
                        .line(reference.line)
                        .contains(&format!("{owner}::{name}"))
                }) || module.rust_uses.iter().any(|imp| {
                    imp.local == name && !self.external_rust(&imp.source)
                        || imp.name == "*"
                            && imp
                                .source
                                .strip_suffix("::*")
                                .is_some_and(|src| node.owner().map(tail) == Some(tail(src)))
                }));
        }
        if node.file == reference.file || !re!(r"^[A-Za-z_]\w*$").is_match(name) {
            return true;
        }
        let mut matching = module.rust_uses.iter().filter(|imp| imp.local == name);
        let bound = matching.clone().any(|imp| !self.external_rust(&imp.source));
        if !bound && matching.any(|imp| self.external_rust(&imp.source)) && node.kind != "method" {
            return false;
        }
        let glob = module.rust_uses.iter().any(|imp| {
            imp.name == "*"
                && !self.external_rust(&imp.source)
                && imp.source.strip_suffix("::*").is_some_and(|src| {
                    tail(src) == module_name(&node.file)
                        || tail(src) == "super"
                            && (dirname(&node.file) == dirname(&reference.file)
                                || dirname(&node.file) == dirname(dirname(&reference.file)))
                })
        });
        if RUST_PRELUDE.contains(&name) {
            return bound || glob;
        }
        bound || glob || member(&node.kind) || reference.kind == "imports" || node.kind == "file"
    }

    fn external_rust(&self, path: &str) -> bool {
        let root = path
            .trim_start_matches("::")
            .split("::")
            .next()
            .unwrap_or_default();
        matches!(root, "std" | "core" | "alloc") || self.config.cargo_dependencies.contains(root)
    }
}

fn callable(kind: &str) -> bool {
    matches!(kind, "function" | "method")
}

fn capitalize(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(head) => format!("{}{}", head.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

fn plain(name: &str) -> String {
    name.replace('_', "").to_lowercase()
}

fn test_path(path: &str) -> bool {
    path.split('/')
        .any(|part| matches!(part, "test" | "tests" | "__tests__" | "spec" | "specs"))
        || path.contains(".test.")
        || path.contains(".spec.")
        || path.ends_with("_test.go")
        || path
            .rsplit('/')
            .next()
            .is_some_and(|file| file.starts_with("test_"))
}

fn module_name(file: &str) -> &str {
    let (dir, leaf) = file.rsplit_once('/').unwrap_or(("", file));
    let base = leaf.split('.').next().unwrap_or(leaf);
    if matches!(base, "mod" | "lib" | "main" | "__init__") {
        dir.rsplit('/').next().unwrap_or(dir)
    } else {
        base
    }
}

fn receiver_of<'a>(name: &'a str, site: &'a str, call: bool) -> Option<&'a str> {
    if let Some((receiver, _)) = name.rsplit_once('.').or_else(|| name.rsplit_once("::")) {
        return Some(receiver);
    }
    if call {
        let (prefix, _) = site.split_once(&format!(".{name}"))?;
        let prefix = prefix.trim().trim_end_matches('?');
        if !prefix.is_empty() && !prefix.contains([';', '=', '"', '\'']) {
            return Some(prefix);
        }
    }
    None
}

fn rust_impl_line(module: &Module, line: u32) -> Option<u32> {
    (1..=line).rev().find(|line| {
        re!(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?(?:impl|trait)\b")
            .is_match(module.line(*line))
    })
}

fn rust_go_receiver(reference: &Reference, module: &Module) -> Option<String> {
    let name = &reference.name;
    let line = module.line(reference.line);
    for (offset, _) in line.match_indices(name) {
        let before = line[..offset].trim_end();
        let after = line[offset + name.len()..].trim_start();
        if let Some(receiver) = before.strip_suffix('.') {
            if after.starts_with(['(', '!', ':']) {
                return Some(receiver_head(receiver));
            }
        }
    }
    for next in reference.line.saturating_add(1)..=reference.line.saturating_add(20) {
        let content = module.line(next).trim_start();
        if let Some(method) = content
            .strip_prefix('.')
            .map(str::trim_start)
            .and_then(|s| s.strip_prefix(name))
        {
            if method.trim_start().starts_with(['(', '!', ':']) {
                return Some(receiver_head(line));
            }
        }
    }
    None
}

fn receiver_head(text: &str) -> String {
    let mut tokens = text
        .trim_end_matches(|c: char| matches!(c, '.' | '?' | ';') || c.is_whitespace())
        .chars()
        .rev();
    let mut delimiters = Vec::new();
    let mut name = Vec::new();
    for token in tokens.by_ref() {
        if let Some(opening) = match token {
            ')' => Some('('),
            ']' => Some('['),
            '}' => Some('{'),
            _ => None,
        } {
            delimiters.push(opening);
        } else if let Some(&opening) = delimiters.last() {
            if token == opening {
                delimiters.pop();
            }
        } else if token.is_alphanumeric() || matches!(token, '_' | '.' | ':') {
            name.push(token);
        } else {
            break;
        }
    }
    name.into_iter().rev().collect()
}

fn rust_field_type(raw: &str) -> Option<&str> {
    let mut raw = raw.trim();
    loop {
        let new = re!(r"^(?:&\s*(?:'\w+\s+)?(?:mut\s+)?|(?:Box|Rc|Arc)\s*<\s*|(?:dyn|impl)\s+)")
            .find(raw)
            .map_or(raw, |prefix| &raw[prefix.end()..]);
        if new == raw {
            break;
        }
        raw = new;
    }
    let name = simple_type(raw)?;
    (!RUST_PRELUDE.contains(&name) && name.len() > 1).then_some(name)
}

fn simple_type(text: &str) -> Option<&str> {
    let text = text.trim().trim_start_matches(['*', '&', '[', ']']);
    let text = text.strip_prefix("mut ").unwrap_or(text);
    let name = re!(r"^[\w$]+(?:(?:::|\.)[\w$]+)*").find(text)?;
    Some(tail(name.as_str()))
}

fn infer_binding(
    module: &Module,
    name: &str,
    start: u32,
    end: u32,
    language: &str,
) -> Option<String> {
    (start..=end)
        .rev()
        .find_map(|line| infer_on_line(module.line(line), name, language))
}

fn infer_on_line(line: &str, name: &str, language: &str) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    for (index, _) in line.match_indices(name) {
        let before = &line[..index];
        let after = &line[index + name.len()..];
        if before.ends_with(ident) || after.starts_with(ident) {
            continue;
        }
        let rest = after.trim_start();
        if language == "go" {
            // #1125: composite literals, explicit var types, and typed
            // parameters. A factory or ordinary assignment isn't a type.
            let ty = if let Some(init) = rest.strip_prefix(":=") {
                let Some(ty) = re!(r"^\s*&?([A-Za-z_][\w.]*)\s*\{")
                    .captures(init)
                    .and_then(|capture| capture.get(1))
                else {
                    continue;
                };
                ty.as_str()
            } else if rest.len() < after.len() {
                let ty = rest.trim_start_matches('*');
                let Some(ty) = re!(r"^[A-Za-z_][\w.]*").find(ty).map(|ty| ty.as_str()) else {
                    continue;
                };
                if !before.trim_end().ends_with("var") && !ty.starts_with(char::is_uppercase) {
                    continue;
                }
                ty
            } else {
                continue;
            };
            return Some(tail(ty).to_owned());
        }
        let declared = rest
            .strip_prefix(':')
            .filter(|value| !value.starts_with('='));
        if let Some(ty) = declared
            .map(|value| value.trim_start().trim_start_matches(['\'', '"']))
            .and_then(simple_type)
        {
            return Some(ty.to_owned());
        }
        if let Some(init) = rest.strip_prefix(":=").or_else(|| rest.strip_prefix('=')) {
            let init = init.trim_start().trim_start_matches('&');
            let new = init.strip_prefix("new ");
            let expression = new.unwrap_or(init);
            if let Some(ty) = simple_type(expression) {
                if new.is_some()
                    || ty.starts_with(char::is_uppercase) && expression.contains(['(', '{'])
                    || language == "rust" && expression.contains("::")
                {
                    let ty = if language == "rust" {
                        expression.split("::").next().unwrap_or(ty)
                    } else {
                        ty
                    };
                    return Some(ty.to_owned());
                }
            }
        }
    }
    None
}

fn locally_bound(
    name: &str,
    reference: &Reference,
    module: &Module,
    scope: Option<&Symbol>,
) -> bool {
    let Some(scope) = scope.filter(|n| callable(&n.kind)) else {
        return false;
    };
    let header = scope
        .signature
        .as_deref()
        .unwrap_or_else(|| module.line(scope.start));
    let parameter = header
        .split_once('(')
        .and_then(|(_, tail)| tail.split_once(')'))
        .is_some_and(|(params, _)| {
            params
                .split(',')
                .any(|param| param.trim().split([':', '=', ' ']).next() == Some(name))
        });
    parameter
        || (scope.start..=reference.line).any(|line| {
            module
                .line(line)
                .split_once('=')
                .is_some_and(|(left, _)| left.split_whitespace().last() == Some(name))
        })
}

fn overlap(receiver: &str, symbol: &Symbol) -> bool {
    let Some(owner) = symbol.owner() else {
        return false;
    };
    let words = |text: &str| {
        re!(r"[A-Z]+[a-z]*|[a-z]+|\d+")
            .find_iter(text)
            .map(|word| word.as_str().to_ascii_lowercase())
            .collect::<Vec<_>>()
    };
    let left = words(tail(receiver));
    let right = words(tail(owner));
    left.iter().any(|word| right.contains(word))
}

fn std_method(language: &str, method: &str) -> bool {
    match language {
        "go" => GO_STD_METHODS.contains(&method),
        "rust" => RUST_STD_METHODS.contains(&method),
        _ => false,
    }
}

fn proximity(left: &str, right: &str) -> i32 {
    (dirname(left)
        .split('/')
        .zip(dirname(right).split('/'))
        .take_while(|(a, b)| !a.is_empty() && a == b)
        .count() as i32
        * 15)
        .min(80)
}

fn best(resolver: &Resolver<'_>, reference: &Reference, candidates: &[usize]) -> Option<usize> {
    let mut best = None;
    let mut high = -10;
    for &id in candidates {
        let n = &resolver.symbols[id];
        let mut score = proximity(&n.file, &reference.file) * 10;
        score += if n.language == reference.language {
            500
        } else {
            -800
        };
        score += if n.exported { 100 } else { 0 };
        if n.file == reference.file {
            score += 1000 + (200 - n.start.abs_diff(reference.line).min(200)) as i32;
        }
        score += match reference.kind.as_str() {
            "calls" | "decorates" if callable(&n.kind) => 250,
            "decorates" if matches!(n.kind.as_str(), "class" | "interface") => 150,
            "instantiates"
                if matches!(n.kind.as_str(), "class" | "struct" | "union" | "interface") =>
            {
                250
            }
            _ => 0,
        };
        if score > high {
            best = Some(id);
            high = score;
        }
    }
    if candidates.len() == 1 {
        candidates.first().copied()
    } else {
        best
    }
}
