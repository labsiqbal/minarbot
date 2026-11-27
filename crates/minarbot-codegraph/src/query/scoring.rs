//! Adapted from colbymchenry/codegraph v1.6.2 (6560052), src/search/query-utils.ts,
//! src/search/identifier-segments.ts and src/context/index.ts.
//! License: MIT (Copyright (c) 2026 Colby Mchenry).

use std::{collections::HashSet, sync::LazyLock};

const STOP: &str = "the a an and or but in on at to for of with by from is it that this are was be has had have do does did will would could should may might can shall not no all each every how what where when who which why i me my we our you your he she they show give tell been done made used using work works found also into then than just more some such over only out its so up as if look need needs want happen happens affect affected break breaks failing implemented implement code file files function method class type fix bug called";
const SYMBOL_STOP: &str = "make use find call calling get set add any them other same about after before between through during without again further once here there both most very being having doing system wants like change changes changed changing layer handle handles handling incoming outgoing data flow flows level levels request requests response responses implements implementation interface interfaces classes methods trigger triggers affects else failed silently decide decides return returns returned take takes taken check checks checked create creates created read reads write writes written start starts stop stops run runs running";
const SEGMENT_STOP: &str = "change changes check classes detail details directory error errors example examples folder functions issue issues line lines methods name names problem problems project question questions rename test tests types update value values warning warnings working write writing";

fn stop(word: &str) -> bool {
    static STOP_WORDS: LazyLock<HashSet<&str>> =
        LazyLock::new(|| STOP.split_whitespace().collect());
    STOP_WORDS.contains(word)
}

pub(super) fn symbols(query: &str) -> Vec<String> {
    static SYMBOL_STOPS: LazyLock<HashSet<&str>> =
        LazyLock::new(|| SYMBOL_STOP.split_whitespace().collect());
    static PATTERNS: LazyLock<[regex::Regex; 5]> = LazyLock::new(|| {
        [
            r"(?-u:\b)(?:[A-Z][a-z]+(?:[A-Z][a-z]*)*|[a-z]+(?:[A-Z][a-z]*)+)(?-u:\b)",
            r"(?i-u:\b[a-z][a-z0-9]*(?:_[a-z0-9]+)+\b)",
            r"(?-u:\b[A-Z]{2,}\b)",
            r"(?-u:\b[a-zA-Z][a-zA-Z0-9]*(?:\.[a-zA-Z][a-zA-Z0-9]*)+\b)",
            r"(?-u:\b[a-z][a-z0-9]{2,}\b)",
        ]
        .map(crate::literal_regex)
    });
    let mut seen = HashSet::new();
    PATTERNS
        .iter()
        .flat_map(|pattern| pattern.find_iter(query))
        .flat_map(|hit| std::iter::once(hit.as_str()).chain(hit.as_str().split('.')))
        .filter(|word| {
            let lower = word.to_ascii_lowercase();
            (2..=128).contains(&word.len())
                && !stop(&lower)
                && !SYMBOL_STOPS.contains(lower.as_str())
                && seen.insert(*word)
        })
        .take(32)
        .map(str::to_owned)
        .collect()
}

pub(super) fn segment_terms(query: &str) -> Vec<String> {
    static SEGMENT_STOPS: LazyLock<HashSet<&str>> =
        LazyLock::new(|| SEGMENT_STOP.split_whitespace().collect());
    terms(query, false)
        .into_iter()
        .filter(|term| (4..=24).contains(&term.len()) && !SEGMENT_STOPS.contains(term.as_str()))
        .take(16)
        .collect()
}

pub(super) fn stems(term: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (suffix, min) in [("ing", 6), ("er", 5)] {
        if let Some(base) = term.strip_suffix(suffix).filter(|_| term.len() >= min) {
            out.extend([base.to_owned(), format!("{base}e")]);
            let bytes = base.as_bytes();
            if bytes.len() >= 2 && bytes.last() == bytes.get(bytes.len() - 2) {
                out.push(base[..base.len() - 1].to_owned());
            }
        }
    }
    for (suffix, cut, min, append) in [
        ("tion", 3, 6, ""),
        ("sion", 3, 6, ""),
        ("ment", 4, 7, ""),
        ("ies", 3, 5, "y"),
        ("es", 2, 5, ""),
        ("s", 1, 5, ""),
        ("ed", 1, 5, ""),
        ("ed", 2, 5, ""),
        ("ied", 3, 6, "y"),
    ] {
        if term.len() >= min
            && term.ends_with(suffix)
            && !(suffix == "s" && (term.ends_with("ss") || term.ends_with("es")))
            && !(suffix == "es" && term.ends_with("ies"))
            && !(suffix == "ed" && term.ends_with("eed"))
        {
            if let Some(base) = term.get(..term.len() - cut) {
                out.push(format!("{base}{append}"));
            }
        }
    }
    out.retain(|word| word.len() >= 3 && word != term);
    out
}

fn split_camel(text: &str) -> String {
    static CAMEL: LazyLock<regex::Regex> =
        LazyLock::new(|| crate::literal_regex(r"([a-z])([A-Z])"));
    static ACRONYM: LazyLock<regex::Regex> =
        LazyLock::new(|| crate::literal_regex(r"([A-Z]+)([A-Z][a-z])"));
    ACRONYM
        .replace_all(&CAMEL.replace_all(text, "${1} ${2}"), "${1} ${2}")
        .into_owned()
}

pub(super) fn terms(query: &str, include_stems: bool) -> Vec<String> {
    static COMPOUND: LazyLock<regex::Regex> = LazyLock::new(|| {
        crate::literal_regex(
            r"(?-u:\b)(?:[a-zA-Z][a-zA-Z0-9]*(?:[A-Z][a-z]+)+|[A-Z][a-z]+(?:[A-Z][a-z]*)+|[a-zA-Z][a-zA-Z0-9]*(?:_[a-zA-Z0-9]+)+)(?-u:\b)",
        )
    });
    let split = split_camel(query);
    let mut seen = HashSet::new();
    let mut out: Vec<_> = COMPOUND
        .find_iter(query)
        .map(|term| term.as_str())
        .chain(split.split(|ch: char| !ch.is_ascii_alphanumeric()))
        .filter(|term| (3..=128).contains(&term.len()))
        .map(str::to_ascii_lowercase)
        .filter(|term| !stop(term) && seen.insert(term.clone()))
        .take(32)
        .collect();
    if include_stems {
        let variants: Vec<_> = out
            .iter()
            .flat_map(|word| stems(word))
            .filter(|term| !stop(term) && seen.insert(term.clone()))
            .take(32)
            .collect();
        out.extend(variants);
    }
    out
}

pub(super) fn distinctive(word: &str) -> bool {
    word.bytes()
        .any(|byte| byte == b'_' || byte.is_ascii_digit())
        || word
            .as_bytes()
            .windows(2)
            .any(|pair| pair[0].is_ascii_lowercase() && pair[1].is_ascii_uppercase())
        || (word.len() >= 2 && word.bytes().all(|byte| byte.is_ascii_uppercase()))
}

pub(super) fn kind(kind: &str) -> f64 {
    match kind {
        "function" | "method" => 10.0,
        "interface" | "protocol" | "trait" | "route" => 9.0,
        "class" | "component" => 8.0,
        "type_alias" | "struct" | "union" => 6.0,
        "enum" => 5.0,
        "module" | "namespace" => 4.0,
        "field" | "property" | "constant" | "enum_member" => 3.0,
        "variable" => 2.0,
        "import" | "export" => 1.0,
        _ => 0.0,
    }
}

pub(super) fn name(name: &str, query: &str) -> f64 {
    let name = name.to_lowercase();
    let query_tokens: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
    let joined = query_tokens.concat();
    if joined.is_empty() {
        return 0.0;
    }
    if name == joined {
        return 80.0;
    }
    if query_tokens.len() > 1 && query_tokens.contains(&name) {
        return 60.0;
    }
    if name.starts_with(&joined) {
        return (10.0 + 30.0 * joined.len() as f64 / name.len() as f64).round();
    }
    let split = split_camel(query).to_lowercase();
    let terms: Vec<_> = split
        .split(|ch: char| ch.is_whitespace() || "_.-".contains(ch))
        .filter(|term| term.len() >= 2)
        .collect();
    if terms.len() > 1 && terms.iter().all(|term| name.contains(term)) {
        return 15.0;
    }
    if name.contains(&joined) {
        return 10.0;
    }
    0.0
}

pub(super) fn is_test(path: &str) -> bool {
    static TEST: LazyLock<regex::Regex> = LazyLock::new(|| {
        crate::literal_regex(
            r"(?i)(?:^|/)(?:tests?|__tests__|specs?|testlib|testing|e2e|testdata|test[-_]?(?:utils?|support)|fakes?|mocks?|__mocks__|stubs|integration|samples?|examples?|benchmarks?|fixtures?|demos|[\w.]+[-_]test(?:s|ing)?)/|(?:^|/)tests?[_.]|[._-](?:tests?|specs?)\.[a-z0-9]+$|(?-i:Test|Tests|TestCase|Tester|Spec|Specs)\.[a-z0-9]+$",
        )
    });
    TEST.is_match(path)
}

pub(super) fn test_query(query: &str) -> bool {
    let query = query.to_ascii_lowercase();
    query.contains("test") || query.contains("spec")
}

#[test]
fn ranks_test_support_as_test_code() -> crate::Result<()> {
    let path = "app/test-support/smoke/browser.mjs";
    assert!(is_test(path));
    assert!(is_test("src/browser/tests.rs"));
    assert!(PathRank::new("browser evaluation").score(path) < 0.0);
    assert!(PathRank::new("browser test").score(path) > 0.0);
    Ok(())
}

pub(super) struct PathRank {
    words: Vec<Vec<String>>,
    test_query: bool,
}

impl PathRank {
    pub fn new(query: &str) -> Self {
        Self {
            words: query
                .split_whitespace()
                .take(32)
                .map(|word| terms(word, false))
                .collect(),
            test_query: test_query(query),
        }
    }

    pub fn score(&self, path: &str) -> f64 {
        let lower = path.to_lowercase();
        let (dir, name) = lower.rsplit_once('/').unwrap_or(("", &lower));
        let mut score = 0.0;
        for word in &self.words {
            if word.iter().any(|term| name.contains(term)) {
                score += 10.0;
            }
            if word.iter().any(|term| dir.contains(term)) {
                score += 5.0;
            } else if word.iter().any(|term| lower.contains(term)) {
                score += 3.0;
            }
        }
        if !self.test_query && is_test(path) {
            score -= 15.0;
        }
        score
    }
}
