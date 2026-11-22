//! Adapted from colbymchenry/codegraph v1.6.2 (6560052),
//! src/extraction/commonjs-requires.ts. License: MIT (Copyright (c) 2026 Colby Mchenry).

use super::Walker;
use std::collections::HashSet;
use tree_sitter::Node;

impl<'t> Walker<'t> {
    pub(super) fn commonjs_imports(&mut self, root: Node) {
        if !self.src.contains("require") {
            return;
        }
        let mut seen = HashSet::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "call_expression" {
                if let Some(name) = self.local_require(node) {
                    if seen.insert(name.to_owned()) {
                        self.push_ref(0, name, "imports", node);
                    }
                }
            }
            let mut cursor = node.walk();
            let children = node.named_children(&mut cursor).collect::<Vec<_>>();
            stack.extend(children.into_iter().rev());
        }
    }

    fn local_require(&self, node: Node) -> Option<&'t str> {
        let callee = node.child_by_field_name("function")?;
        if callee.kind() != "identifier"
            || self.text(callee) != "require"
            || node
                .parent()
                .is_some_and(|parent| parent.kind() == "import_require_clause")
        {
            return None;
        }
        let args = node.child_by_field_name("arguments")?;
        let argument = args.named_child(0)?;
        if args.named_child_count() != 1 || !matches!(argument.kind(), "string" | "template_string")
        {
            return None;
        }
        let raw = self.text(argument);
        let name = raw.get(1..raw.len().checked_sub(1)?)?.trim();
        if name.contains('\n') || argument.kind() == "template_string" && name.contains('$') {
            return None;
        }
        (matches!(name, "." | "..")
            || ["./", "../", "/", "@/", "~/", "#"]
                .iter()
                .any(|prefix| name.starts_with(prefix)))
        .then_some(name)
    }
}
