//! Adapted from colbymchenry/codegraph v1.6.2 (6560052),
//! codegraph-kernel/src/langs.rs and src/extraction/grammars.ts.
//! License: MIT (Copyright (c) 2026 Colby Mchenry).
//!
//! Grammars are pinned to upstream kernel revisions.

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
        "java" if cfg!(feature = "java") => Some("java"),
        "h" if cfg!(feature = "c") || cfg!(feature = "cpp") => Some("c"),
        "c" if cfg!(feature = "c") => Some("c"),
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "metal" | "cu" | "cuh" if cfg!(feature = "cpp") => {
            Some("cpp")
        }
        "cs" if cfg!(feature = "csharp") => Some("csharp"),
        "rb" | "rake" if cfg!(feature = "ruby") => Some("ruby"),
        "php" | "module" | "install" | "theme" | "inc" if cfg!(feature = "php") => Some("php"),
        "swift" if cfg!(feature = "swift") => Some("swift"),
        "kt" | "kts" if cfg!(feature = "kotlin") => Some("kotlin"),
        "r" if cfg!(feature = "r") => Some("r"),
        "lua" if cfg!(feature = "lua") => Some("lua"),
        "luau" if cfg!(feature = "luau") => Some("luau"),
        "scala" | "sc" if cfg!(feature = "scala") => Some("scala"),
        "dart" if cfg!(feature = "dart") => Some("dart"),
        _ => None,
    }
}

pub(crate) fn detect_source(
    path: &Path,
    language: &'static str,
    source: &str,
) -> Option<&'static str> {
    if language != "c"
        || !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("h"))
    {
        return Some(language);
    }

    use std::sync::LazyLock;
    static CPP: LazyLock<regex::Regex> = LazyLock::new(|| {
        crate::literal_regex(
            r"\bnamespace\b|\bclass\s+\w+\s*[:{]|\b(?:class|struct)\s+[A-Z][A-Z0-9_]+\s+\w+\s*(?:final\s*)?[:{]|\btemplate\s*<|\b(?:public|private|protected)\s*:|\bvirtual\b|\busing\s+(?:namespace\b|\w+\s*=)",
        )
    });
    static BASE: LazyLock<regex::Regex> = LazyLock::new(|| {
        crate::literal_regex(
            r"(?m)^[ 	]*(?:struct|class)\s+[A-Za-z_]\w*\s*(?:final\s*)?:\s*(?:::)?[A-Za-z_]\w*",
        )
    });
    static COMMENT: LazyLock<regex::Regex> =
        LazyLock::new(|| crate::literal_regex(r"(?s)/\*.*?\*/|//[^\n]*"));
    static OBJC: LazyLock<regex::Regex> = LazyLock::new(|| {
        crate::literal_regex(r"@(?:interface|implementation|protocol|synthesize)\b")
    });

    let sample = &source[..source.floor_char_boundary(8192)];
    if CPP.is_match(sample) || BASE.is_match(&COMMENT.replace_all(source, " ")) {
        cfg!(feature = "cpp").then_some("cpp")
    } else if OBJC.is_match(sample) {
        None
    } else {
        cfg!(feature = "c").then_some("c")
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
        #[cfg(feature = "java")]
        "java" => Some(tree_sitter_java::LANGUAGE.into()),
        #[cfg(feature = "c")]
        "c" => Some(tree_sitter_c::LANGUAGE.into()),
        #[cfg(feature = "cpp")]
        "cpp" => Some(tree_sitter_cpp::LANGUAGE.into()),
        #[cfg(feature = "csharp")]
        "csharp" => Some(tree_sitter_c_sharp::LANGUAGE.into()),
        #[cfg(feature = "ruby")]
        "ruby" => Some(tree_sitter_ruby::LANGUAGE.into()),
        #[cfg(feature = "php")]
        "php" => Some(tree_sitter_php::LANGUAGE_PHP.into()),
        #[cfg(feature = "swift")]
        "swift" => Some(tree_sitter_swift::LANGUAGE.into()),
        #[cfg(feature = "kotlin")]
        "kotlin" => Some(from_vendored(tree_sitter_kotlin)),
        #[cfg(feature = "r")]
        "r" => Some(tree_sitter_r::LANGUAGE.into()),
        #[cfg(feature = "lua")]
        "lua" => Some(tree_sitter_lua::LANGUAGE.into()),
        #[cfg(feature = "luau")]
        "luau" => Some(tree_sitter_luau::LANGUAGE.into()),
        #[cfg(feature = "scala")]
        "scala" => Some(tree_sitter_scala::LANGUAGE.into()),
        #[cfg(feature = "dart")]
        "dart" => Some(from_vendored(tree_sitter_dart)),
        _ => None,
    }
}

#[cfg(any(feature = "kotlin", feature = "dart"))]
extern "C" {
    #[cfg(feature = "kotlin")]
    fn tree_sitter_kotlin() -> *const ();
    #[cfg(feature = "dart")]
    fn tree_sitter_dart() -> *const ();
}

#[cfg(any(feature = "kotlin", feature = "dart"))]
fn from_vendored(grammar: unsafe extern "C" fn() -> *const ()) -> Language {
    // SAFETY: grammar is a tree-sitter entry point compiled by build.rs.
    unsafe { tree_sitter_language::LanguageFn::from_raw(grammar) }.into()
}
