//! Adapted from colbymchenry/codegraph v1.6.2 (6560052),
//! codegraph-kernel/src/langs.rs and src/extraction/grammars.ts.
//! License: MIT (Copyright (c) 2026 Colby Mchenry).
//!
//! MVP grammars only. Versions are pinned to upstream kernel revisions.

use std::path::Path;
use tree_sitter::Language;

pub(crate) fn detect(path: &Path) -> Option<&'static str> {
    let (_, extension) = path.file_name()?.to_str()?.rsplit_once('.')?;
    match extension.to_ascii_lowercase().as_str() {
        "rs" if cfg!(feature = "rust") => Some("rust"),
        "ts" | "mts" | "cts" if cfg!(feature = "typescript") => Some("typescript"),
        "tsx" if cfg!(feature = "typescript") => Some("tsx"),
        "js" | "mjs" | "cjs" | "xsjs" | "xsjslib" if cfg!(feature = "javascript") => {
            Some("javascript")
        }
        "jsx" if cfg!(feature = "javascript") => Some("jsx"),
        "py" | "pyw" if cfg!(feature = "python") => Some("python"),
        "go" if cfg!(feature = "go") => Some("go"),
        _ => None,
    }
}

pub(crate) fn grammar_for(language: &str) -> Option<Language> {
    match language {
        #[cfg(feature = "rust")]
        "rust" => Some(tree_sitter_rust::LANGUAGE.into()),
        #[cfg(feature = "typescript")]
        "typescript" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        #[cfg(feature = "typescript")]
        "tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        #[cfg(feature = "javascript")]
        "javascript" | "jsx" => Some(tree_sitter_javascript::LANGUAGE.into()),
        #[cfg(feature = "python")]
        "python" => Some(tree_sitter_python::LANGUAGE.into()),
        #[cfg(feature = "go")]
        "go" => Some(tree_sitter_go::LANGUAGE.into()),
        _ => None,
    }
}
