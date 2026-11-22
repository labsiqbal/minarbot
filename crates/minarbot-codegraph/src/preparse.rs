//! Adapted from colbymchenry/codegraph v1.6.2 (6560052), src/extraction/languages/
//! csharp.ts, kotlin.ts and swift.ts.
//! License: MIT (Copyright (c) 2026 Colby Mchenry).

use std::borrow::Cow;

#[cfg(any(
    feature = "c",
    feature = "cpp",
    feature = "csharp",
    feature = "kotlin",
    feature = "swift"
))]
macro_rules! re {
    ($pattern:literal) => {{
        static RE: std::sync::LazyLock<regex::Regex> =
            std::sync::LazyLock::new(|| crate::literal_regex($pattern));
        &*RE
    }};
}

#[cfg(any(feature = "c", feature = "cpp"))]
mod ccpp;

pub(crate) fn prepare<'a>(source: &'a str, _path: &str, language: &str) -> Cow<'a, str> {
    match language {
        #[cfg(any(feature = "c", feature = "cpp"))]
        "c" | "cpp" => Cow::Owned(ccpp::prepare(source, _path, language == "cpp")),
        #[cfg(feature = "csharp")]
        "csharp" => re!(r"(?m)^[ \t]*#[ \t]*(?:if|elif|else|endif)\b[^\r\n]*")
            .replace_all(source, |m: &regex::Captures| " ".repeat(m[0].len())),
        #[cfg(feature = "kotlin")]
        "kotlin" => kotlin(source),
        #[cfg(feature = "swift")]
        "swift" => swift(source),
        _ => Cow::Borrowed(source),
    }
}

#[cfg(any(feature = "c", feature = "cpp", feature = "kotlin"))]
fn blank(bytes: &mut [u8]) {
    for byte in bytes {
        if !matches!(byte, b'\r' | b'\n') {
            *byte = b' ';
        }
    }
}

#[cfg(any(feature = "c", feature = "cpp", feature = "kotlin"))]
fn source_string(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}

#[cfg(feature = "kotlin")]
fn kotlin(source: &str) -> Cow<'_, str> {
    if ![".(", "constructor", " if", "..<", "$$", "?.()"]
        .iter()
        .any(|s| source.contains(s))
    {
        return Cow::Borrowed(source);
    }
    let mut bytes = source.as_bytes().to_vec();
    for m in re!(r"\b(?:[A-Za-z_]\w*(?:<[^<>()\n]*>)?\.)+[ \t]*\(").find_iter(source) {
        let receiver = m
            .as_str()
            .trim_end_matches('(')
            .trim_end()
            .trim_end_matches('.');
        if let Some((qualifier, _)) = receiver.rsplit_once('.') {
            blank(&mut bytes[m.start()..m.start() + qualifier.len() + 1]);
        }
    }
    for cap in re!(r"\bclass\s+[A-Za-z_]\w*(?:\s*<[^<>{}]*(?:<[^<>{}]*>[^<>{}]*)*>)?[ \t]*\r?\n(?:[ \t]*//[^\n]*\n)*[ \t]*((?:(?:public|private|protected|internal|actual|expect)\s+|@[\w.]+(?:\([^)]*\))?\s+)*constructor)\s*\(").captures_iter(source) {
        if let Some(ctor) = cap.get(1) {
            blank(&mut bytes[ctor.range()]);
        }
    }
    for cap in re!(r#"(?m)^[ \t]*!?(?:is|in)[ \t]+([^\n]*?[\w>?)\]'"])([ \t]+if)[ 	(]"#)
        .captures_iter(source)
    {
        if let (Some(condition), Some(guard)) = (cap.get(1), cap.get(2)) {
            if !condition.as_str().contains("->") {
                blank(&mut bytes[guard.range()]);
                bytes[guard.start()] = b',';
            }
        }
    }
    for m in re!(r"\.\.<").find_iter(source) {
        bytes[m.end() - 1] = b' ';
    }
    for m in re!(r#"\${2,}""#).find_iter(source) {
        if !source[..m.start()]
            .ends_with(|c: char| c.is_alphanumeric() || matches!(c, '_' | '$' | '"'))
        {
            blank(&mut bytes[m.start()..m.end() - 1]);
        }
    }
    for cap in re!(r"[\w>](\?\.\(\))\s*->").captures_iter(source) {
        if let Some(receiver) = cap.get(1) {
            bytes[receiver.range()].copy_from_slice(b".( )");
        }
    }
    Cow::Owned(source_string(bytes))
}

#[cfg(feature = "swift")]
fn swift(source: &str) -> Cow<'_, str> {
    let continuation = re!(r"^[ \t]*(&)[ \t]+\S");
    if !source.lines().any(|line| continuation.is_match(line)) {
        return Cow::Borrowed(source);
    }
    let head = re!(
        r"^[ \t]+(?:@[\w.]+(?:\([^)]*\))?\s*)*(?:[a-z]+(?:\([a-z]+\))?\s+)*(?:typealias|let|var)\s"
    );
    let mut joined = Vec::<String>::new();
    let mut composition = false;
    for line in source.split('\n') {
        if let Some(amp) = continuation.captures(line).and_then(|cap| cap.get(1)) {
            if let Some(prev) = joined.last_mut().filter(|_| composition) {
                let body = prev.trim_end_matches('\r');
                if !body.contains("//")
                    && body.ends_with(|c: char| c.is_ascii_alphanumeric() || "_>)]?!".contains(c))
                {
                    prev.insert(body.len(), '&');
                    joined.push(format!("{}{}", &line[..amp.start()], &line[amp.end()..]));
                    continue;
                }
            }
        } else {
            composition = head.is_match(line);
        }
        joined.push(line.to_owned());
    }
    Cow::Owned(joined.join("\n"))
}
