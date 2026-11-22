//! Ported from colbymchenry/codegraph v1.6.2 (6560052), codegraph-kernel/src/rlang.rs.
//! License: MIT (Copyright (c) 2026 Colby Mchenry).
//!
//! R extraction with assignments and R6 and S4 declarations.
//! Coordinates use UTF-16 columns. Recovered syntax is reported in the file index.

use crate::buffers::{BoolFlags, EdgeRow, NodeRow, RefRow, Tables, FLAG_IS_EXPORTED};
use crate::ids;
use crate::textutil as util;
use regex::Regex;
use std::sync::OnceLock;
use tree_sitter::{Node, Parser};

/// CONSTANT_NAME (r.ts:43) — ALL_CAPS or DOTTED.CAPS top-level assignment.
fn constant_name_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| crate::literal_regex(r"^[A-Z][A-Z0-9._]*$"))
}

/// ASSIGN_LEFT / ASSIGN_RIGHT (r.ts:37-38).
fn is_assign_left(op: &str) -> bool {
    matches!(op, "<-" | "<<-" | "=")
}
fn is_assign_right(op: &str) -> bool {
    matches!(op, "->" | "->>")
}

/// IMPORT_FNS (r.ts:39) — `source` is checked alongside (r.ts:189).
fn is_import_fn(name: &str) -> bool {
    matches!(
        name,
        "library" | "require" | "requireNamespace" | "loadNamespace"
    )
}
/// CLASS_FNS (r.ts:40).
fn is_class_fn(name: &str) -> bool {
    matches!(name, "setClass" | "setRefClass" | "R6Class" | "ggproto")
}
/// GENERIC_FNS (r.ts:41).
fn is_generic_fn(name: &str) -> bool {
    matches!(name, "setGeneric" | "setMethod")
}

struct Scope {
    row: u32,
    kind: &'static str,
    name: String,
}

pub struct Walker<'t> {
    src: &'t str,
    file_path: &'t str,
    line_starts: Vec<usize>,
    node_id_allocator: ids::NodeIdAllocator,
    tables: Tables,
    stack: Vec<Scope>,
}

pub fn extract(file_path: &str, source: &str) -> Result<(Tables, bool), String> {
    let grammar = crate::langs::grammar_for("r").ok_or("no r grammar")?;
    let mut parser = Parser::new();
    parser
        .set_language(&grammar)
        .map_err(|e| format!("set_language(r) failed: {e}"))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| "parser returned null tree".to_string())?;
    let recovered = tree.root_node().has_error();

    let mut w = Walker {
        src: source,
        file_path,
        line_starts: util::line_starts(source),
        node_id_allocator: ids::NodeIdAllocator::default(),
        tables: Tables::default(),
        stack: Vec::new(),
    };

    // File node (tree-sitter.ts:508-521) — the only node with isExported set.
    let line_count = source.bytes().filter(|b| *b == b'\n').count() as u32 + 1;
    let base_name = file_path.rsplit(['/', '\\']).next().unwrap_or(file_path);
    let mut flags = BoolFlags::default();
    flags.set(FLAG_IS_EXPORTED, false);
    let file_id = ids::file_node_id(file_path);
    let name_ref = base_name.to_owned();
    let qn_ref = file_path.to_owned();
    w.tables.push_node(NodeRow {
        kind: "file",
        visibility: 0,
        flags,
        start_line: 1,
        end_line: line_count,
        start_column: 0,
        end_column: 0,
        name: name_ref,
        qualified_name: qn_ref,
        id: file_id,
        docstring: None,
        signature: None,
        decorators: None,
        type_parameters: None,
        return_type: None,
    });
    w.stack.push(Scope {
        row: 0,
        kind: "file",
        name: base_name.to_string(),
    });

    // extractFilePackage returns null (no packageTypes); both end-of-file
    // flushes are no-ops for R (no fnRefSpec, VALUE_REF_LANGS gate).
    w.visit(tree.root_node());
    w.stack.pop();

    Ok((w.tables, recovered))
}

impl<'t> Walker<'t> {
    fn text(&self, node: Node) -> &'t str {
        &self.src[node.byte_range()]
    }
    fn line_of(&self, node: Node) -> u32 {
        node.start_position().row as u32 + 1
    }
    fn col_of(&self, node: Node) -> u32 {
        util::col16(
            self.src,
            &self.line_starts,
            node.start_position().row,
            node.start_byte(),
        )
    }
    fn end_col_of(&self, node: Node) -> u32 {
        util::col16(
            self.src,
            &self.line_starts,
            node.end_position().row,
            node.end_byte(),
        )
    }
    fn top_row(&self) -> u32 {
        self.stack.last().map(|s| s.row).unwrap_or(0)
    }

    fn push_ref_at(&mut self, from_row: u32, name: &str, kind: &'static str, node: Node) {
        let name_ref = name.to_owned();
        self.tables.refs.push(RefRow {
            from_idx: from_row,
            kind,
            line: self.line_of(node),
            column: self.col_of(node),
            reference_name: name_ref,
            candidates: None,
        });
    }

    // --- createNode (tree-sitter.ts:1308) ---------------------------------
    // R extras carry only `signature` (or nothing): no docstring, visibility,
    // isStatic/isAsync/isExported, returnType, decorators — ever. The endLine
    // body-extension is dead (no resolveBody hook).

    fn create_node(
        &mut self,
        kind: &'static str,
        name: &str,
        node: Node<'t>,
        signature: Option<&str>,
    ) -> Option<u32> {
        if name.is_empty() {
            return None;
        }
        let start_line = self.line_of(node);
        let column = self.col_of(node);
        let id = self
            .node_id_allocator
            .generate(self.file_path, kind, name, start_line, column);

        // buildQualifiedName (tree-sitter.ts:1447-1460): non-file stack names
        // joined `::` (namespacePrefix is always empty for R).
        let qualified = {
            let mut parts: Vec<&str> = Vec::new();
            for s in &self.stack {
                if s.kind != "file" {
                    parts.push(&s.name);
                }
            }
            let mut qn = parts.join("::");
            if !qn.is_empty() {
                qn.push_str("::");
            }
            qn.push_str(name);
            qn
        };

        let name_ref = name.to_owned();
        let sig_ref = signature.map(str::to_owned);
        let row = self.tables.push_node(NodeRow {
            kind,
            visibility: 0,
            flags: BoolFlags::default(),
            start_line,
            end_line: node.end_position().row as u32 + 1,
            start_column: self.col_of(node),
            end_column: self.end_col_of(node),
            name: name_ref,
            qualified_name: qualified,
            id,
            docstring: None,
            signature: sig_ref,
            decorators: None,
            type_parameters: None,
            return_type: None,
        });

        // Containment edge from the stack top (always non-empty — file node).
        let parent_row = self.top_row();
        self.tables.edges.push(EdgeRow {
            source_idx: parent_row,
            target_idx: row,
            kind: "contains",
            line: None,
            column: None,
            metadata_json: None,
        });

        Some(row)
    }

    // --- r.ts helper transcriptions ---------------------------------------

    /// calleeName (r.ts:46-55): bare identifier text, or a
    /// namespace_operator's `rhs` field text (`pkg::fn` → `fn`) — so
    /// `methods::setClass` etc. trigger the special branches. Everything else
    /// (extract_operator, subset2, string, call, return…) → None.
    fn callee_name(&self, call: Node<'t>) -> Option<&'t str> {
        let f = call.child_by_field_name("function")?;
        match f.kind() {
            "identifier" => Some(self.text(f)),
            "namespace_operator" => f.child_by_field_name("rhs").map(|rhs| self.text(rhs)),
            _ => None,
        }
    }

    /// firstArgValue (r.ts:58-67): the FIRST `argument`-typed named child of
    /// the `arguments` field → its `value` field (named arguments are NOT
    /// skipped — `library(help = docpkg)` imports "docpkg", preserved bug).
    fn first_arg_value(&self, call: Node<'t>) -> Option<Node<'t>> {
        let args = call.child_by_field_name("arguments")?;
        let mut cursor = args.walk();
        for arg in args.named_children(&mut cursor) {
            if arg.kind() != "argument" {
                continue;
            }
            return arg.child_by_field_name("value");
        }
        None
    }

    /// literalOrIdentifier (r.ts:70-81): identifier text (backticks kept), a
    /// string's first string_content text, `Some("")` for an empty string
    /// literal (falsy downstream, like null), None otherwise.
    fn literal_or_identifier(&self, node: Option<Node<'t>>) -> Option<&'t str> {
        let node = node?;
        match node.kind() {
            "identifier" => Some(self.text(node)),
            "string" => {
                let mut cursor = node.walk();
                for c in node.named_children(&mut cursor) {
                    if c.kind() == "string_content" {
                        return Some(self.text(c));
                    }
                }
                Some("")
            }
            _ => None,
        }
    }

    // --- the visitNode walk (tree-sitter.ts:936-953, 1248, 1295-1301) -----
    // Hook first (consumed → return; scanFnRefSubtree is a no-op for R), then
    // the ladder — only callTypes can match, with children still recursed —
    // else plain recursion over namedChildren in order.

    fn visit(&mut self, node: Node<'t>) {
        stack_guard!();
        if self.hook(node) {
            return;
        }
        if node.kind() == "call" {
            self.extract_call(node);
        }
        let mut cursor = node.walk();
        let children: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
        for child in children {
            self.visit(child);
        }
    }

    /// The visitNode hook (r.ts:180-309). Returns true when consumed.
    fn hook(&mut self, node: Node<'t>) -> bool {
        stack_guard!();
        match node.kind() {
            "call" => self.hook_call(node),
            "binary_operator" => self.hook_binary_operator(node),
            _ => false,
        }
    }

    fn hook_call(&mut self, node: Node<'t>) -> bool {
        stack_guard!();
        let fname = match self.callee_name(node) {
            Some(f) => f,
            None => return false,
        };

        // library(dplyr) / require(stats) / requireNamespace("jsonlite") /
        // source("helpers.R") (r.ts:189-208). A dynamic/missing/empty first
        // arg is consumed SILENTLY — nothing recorded, subtree never visited.
        if is_import_fn(fname) || fname == "source" {
            let module = match self.literal_or_identifier(self.first_arg_value(node)) {
                Some(m) if !m.is_empty() => m,
                _ => return true,
            };
            // signature: whole call text .trim().slice(0, 100) — UTF-16 slice.
            // (A call node's text starts at the callee and ends at `)`, so
            // trim() never has anything to strip on reachable inputs.)
            let (sig, _) = util::slice_utf16(self.text(node).trim(), 100);
            let module = module.to_string();
            let imp = self.create_node("import", &module, node, Some(&sig));
            if imp.is_some() && !self.stack.is_empty() {
                let parent_row = self.top_row();
                self.push_ref_at(parent_row, &module, "imports", node);
            }
            return true;
        }

        // setClass("Patient", …) / setRefClass / R6Class / ggproto
        // (r.ts:211-221). A falsy name FALLS THROUGH to the generic call —
        // `ggproto(NULL, Geom, …)` emits `calls ggproto` + file-scope body
        // leak (asymmetric with imports, preserved).
        if is_class_fn(fname) {
            let name = match self.literal_or_identifier(self.first_arg_value(node)) {
                Some(n) if !n.is_empty() => n.to_string(),
                _ => return false,
            };
            if let Some(cls_row) = self.create_node("class", &name, node, None) {
                self.stack.push(Scope {
                    row: cls_row,
                    kind: "class",
                    name,
                });
                self.extract_class_members(node, cls_row);
                self.stack.pop();
            }
            return true;
        }

        // setGeneric("describe", …) / setMethod("describe", "Patient", fn)
        // (r.ts:224-249): function node named by the first arg; signature and
        // body from the FIRST argument (any position) whose value is a
        // function_definition.
        if is_generic_fn(fname) {
            let name = match self.literal_or_identifier(self.first_arg_value(node)) {
                Some(n) if !n.is_empty() => n.to_string(),
                _ => return false,
            };
            let mut impl_node: Option<Node<'t>> = None;
            if let Some(args) = node.child_by_field_name("arguments") {
                let mut cursor = args.walk();
                for a in args.named_children(&mut cursor) {
                    if a.kind() != "argument" {
                        continue;
                    }
                    if let Some(v) = a.child_by_field_name("value") {
                        if v.kind() == "function_definition" {
                            impl_node = Some(v);
                            break;
                        }
                    }
                }
            }
            let params_text = impl_node
                .and_then(|i| i.child_by_field_name("parameters"))
                .map(|p| self.text(p));
            let fn_row = self.create_node("function", &name, node, params_text);
            let body = impl_node.and_then(|i| i.child_by_field_name("body"));
            if let (Some(row), Some(body)) = (fn_row, body) {
                self.stack.push(Scope {
                    row,
                    kind: "function",
                    name,
                });
                self.visit(body);
                self.stack.pop();
            }
            return true;
        }

        false // ordinary call — generic extraction records the edge
    }

    fn hook_binary_operator(&mut self, node: Node<'t>) -> bool {
        stack_guard!();
        let op = match node.child_by_field_name("operator") {
            Some(o) => self.text(o),
            None => return false,
        };
        let lhs = node.child_by_field_name("lhs");
        let rhs = node.child_by_field_name("rhs");

        // name <- function(…) — ANY scope (r.ts:267-279). Body walked through
        // the hook-aware visit (visitFunctionBody never runs for R).
        if is_assign_left(op) {
            if let (Some(lhs), Some(rhs)) = (lhs, rhs) {
                if lhs.kind() == "identifier" && rhs.kind() == "function_definition" {
                    let params_text = rhs.child_by_field_name("parameters").map(|p| self.text(p));
                    let name = self.text(lhs).to_string();
                    let fn_row = self.create_node("function", &name, node, params_text);
                    let body = rhs.child_by_field_name("body");
                    if let (Some(row), Some(body)) = (fn_row, body) {
                        self.stack.push(Scope {
                            row,
                            kind: "function",
                            name,
                        });
                        self.visit(body);
                        self.stack.pop();
                    }
                    return true;
                }
            }
        }

        let top_level = node
            .parent()
            .map(|p| p.kind() == "program")
            .unwrap_or(false);

        // Top-level value assignments → variable/constant (r.ts:284-296);
        // the class-definition idiom suppresses the twin variable node but the
        // rhs is ALWAYS visited.
        if top_level && is_assign_left(op) {
            if let (Some(lhs), Some(rhs)) = (lhs, rhs) {
                if lhs.kind() == "identifier" {
                    let rhs_callee = if rhs.kind() == "call" {
                        self.callee_name(rhs)
                    } else {
                        None
                    };
                    let suppressed = rhs_callee
                        .map(|c| is_class_fn(c) || is_generic_fn(c))
                        .unwrap_or(false);
                    if !suppressed {
                        let name = self.text(lhs);
                        let kind = if constant_name_re().is_match(name) {
                            "constant"
                        } else {
                            "variable"
                        };
                        self.create_node(kind, name, node, None);
                    }
                    self.visit(rhs);
                    return true;
                }
            }
        }

        // value -> name / value ->> name (r.ts:298-303).
        if top_level && is_assign_right(op) {
            if let (Some(lhs), Some(rhs)) = (lhs, rhs) {
                if rhs.kind() == "identifier" {
                    let name = self.text(rhs);
                    let kind = if constant_name_re().is_match(name) {
                        "constant"
                    } else {
                        "variable"
                    };
                    self.create_node(kind, name, node, None);
                    self.visit(lhs);
                    return true;
                }
            }
        }

        false
    }

    /// extractClassMembers (r.ts:110-163): arguments in source order with a
    /// positional counter — ggproto's 2nd positional identifier and
    /// `inherit`/`contains` named args become `extends` refs (from the CLASS
    /// row, positioned at the VALUE node); named function_definition args and
    /// `list(…)` entries become methods. Non-method argument subtrees are
    /// NEVER visited (`representation(…)`, `signature(…)` invisible).
    fn extract_class_members(&mut self, class_call: Node<'t>, class_row: u32) {
        stack_guard!();
        let args = match class_call.child_by_field_name("arguments") {
            Some(a) => a,
            None => return,
        };
        let mut positional = 0u32;
        let mut cursor = args.walk();
        let arg_nodes: Vec<Node<'t>> = args.named_children(&mut cursor).collect();
        for arg in arg_nodes {
            if arg.kind() != "argument" {
                continue;
            }
            let arg_name = arg.child_by_field_name("name");
            let value = arg.child_by_field_name("value");
            let arg_name = match arg_name {
                None => {
                    positional += 1;
                    if positional == 2 {
                        if let Some(v) = value {
                            if v.kind() == "identifier" {
                                let parent = self.text(v).to_string();
                                self.push_ref_at(class_row, &parent, "extends", v);
                            }
                        }
                    }
                    continue;
                }
                Some(n) => n,
            };
            let arg_name_text = self.text(arg_name);
            // R6 `inherit = Parent` / S4 `contains = "Parent"` — a falsy
            // resolution (`inherit = pkg::Parent`, empty string) emits nothing.
            if (arg_name_text == "inherit" || arg_name_text == "contains") && value.is_some() {
                if let Some((parent, value)) = self.literal_or_identifier(value).zip(value) {
                    if !parent.is_empty() {
                        let parent = parent.to_string();
                        self.push_ref_at(class_row, &parent, "extends", value);
                    }
                }
                continue;
            }
            // Direct named function argument (ggproto methods).
            if let Some(v) = value {
                if v.kind() == "function_definition" {
                    self.emit_method_arg(arg);
                    continue;
                }
                // list(…) of named function arguments (R5/R6 methods).
                if v.kind() == "call" && self.callee_name(v) == Some("list") {
                    if let Some(list_args) = v.child_by_field_name("arguments") {
                        let mut lc = list_args.walk();
                        let entries: Vec<Node<'t>> = list_args.named_children(&mut lc).collect();
                        for entry in entries {
                            if entry.kind() == "argument" {
                                self.emit_method_arg(entry);
                            }
                        }
                    }
                }
            }
        }
    }

    /// emitMethodArg (r.ts:84-98): `name = function(…)` argument entry → a
    /// `method` node positioned at the ARGUMENT node, signature from the raw
    /// parameters text, body walked hook-aware inside the method scope.
    fn emit_method_arg(&mut self, entry: Node<'t>) {
        stack_guard!();
        let entry_name = match entry.child_by_field_name("name") {
            Some(n) => n,
            None => return,
        };
        let entry_value = match entry.child_by_field_name("value") {
            Some(v) if v.kind() == "function_definition" => v,
            _ => return,
        };
        let params_text = entry_value
            .child_by_field_name("parameters")
            .map(|p| self.text(p));
        let name = self.text(entry_name).to_string();
        let method_row = self.create_node("method", &name, entry, params_text);
        let body = entry_value.child_by_field_name("body");
        if let (Some(row), Some(body)) = (method_row, body) {
            self.stack.push(Scope {
                row,
                kind: "method",
                name,
            });
            self.visit(body);
            self.stack.pop();
        }
    }

    // --- extractCall (tree-sitter.ts:3684, 4313, 4518-4532, 4572-4580) ----
    // R call nodes reach the generic tail: callee = RAW `function`-field text
    // verbatim (member branch unreachable, cpp recoveries language-gated),
    // then the parenthesized-conversion regex. No skipChildren — inner calls
    // are visited by the ladder's recursion afterward.

    fn extract_call(&mut self, node: Node<'t>) {
        if self.stack.is_empty() {
            return;
        }
        let caller_row = self.top_row();
        let func = node
            .child_by_field_name("function")
            .or_else(|| node.named_child(0));
        let func = match func {
            Some(f) => f,
            None => return,
        };
        let mut callee: &str = self.text(func);
        if let Some(caps) = util::paren_conversion().captures(callee) {
            if let Some(inner) = caps.get(1) {
                callee = &callee[inner.range()];
            }
        }
        if callee.is_empty() {
            return;
        }
        let callee = callee.to_string();
        self.push_ref_at(caller_row, &callee, "calls", node);
    }
}
