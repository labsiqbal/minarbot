//! Adapted from colbymchenry/codegraph v1.6.2 (6560052), src/extraction/languages/c-cpp.ts.
//! License: MIT (Copyright (c) 2026 Colby Mchenry).

use super::{blank, source_string};
use regex::Regex;
use std::ops::Range;

fn blank_groups(source: &mut String, pattern: &Regex, group: usize) {
    let ranges = pattern
        .captures_iter(source)
        .filter_map(|cap| cap.get(group).map(|m| m.range()))
        .collect::<Vec<_>>();
    for range in ranges {
        blank_range(source, range);
    }
}

fn blank_range(source: &mut String, range: Range<usize>) {
    let mut spaces = source.as_bytes()[range.clone()].to_vec();
    blank(&mut spaces);
    source.replace_range(range, &source_string(spaces));
}

fn directive(line: &str) -> Option<&str> {
    let body = line.trim_start().strip_prefix('#')?.trim_start();
    let end = body
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(body.len());
    Some(&body[..end])
}

fn lines(source: &str) -> Vec<Range<usize>> {
    source
        .split_inclusive('\n')
        .scan(0, |start, line| {
            let range = *start..*start + line.len();
            *start = range.end;
            Some(range)
        })
        .collect()
}

fn raw_strings(source: &str) -> (String, Vec<Range<usize>>) {
    if !source.contains("R\"") {
        return (source.to_owned(), Vec::new());
    }
    let tokens = re!(
        r#"(?s)//[^\r\n]*|/\*.*?(?:\*/|$)|\b(?:u8|[LuU])?R"([^ \t\x0B\x0C\r\n()\\]{0,16})\(|"(?:\\.|[^"\\])*(?:"|$)|'(?:\\.|[^'\\])*(?:'|$)"#
    );
    let mut masked = source.as_bytes().to_vec();
    let mut ranges = Vec::new();
    let mut offset = 0;
    while let Some(cap) = tokens.captures_at(source, offset) {
        let Some(token) = cap.get(0) else { break };
        offset = token.end();
        if let Some(delimiter) = cap.get(1) {
            let closer = format!("){}\"", delimiter.as_str());
            offset = source[offset..]
                .find(&closer)
                .map_or(source.len(), |end| offset + end + closer.len());
            let range = token.start()..offset;
            for byte in &mut masked[range.clone()] {
                if !matches!(*byte, b'\r' | b'\n') {
                    *byte = 0;
                }
            }
            ranges.push(range);
        }
    }
    (source_string(masked), ranges)
}

fn restore_raw(source: &str, out: String, raw: Vec<Range<usize>>) -> String {
    let mut bytes = out.into_bytes();
    for range in raw {
        for (byte, original) in bytes[range.clone()]
            .iter_mut()
            .zip(&source.as_bytes()[range])
        {
            if *byte == 0 {
                *byte = *original;
            }
        }
    }
    source_string(bytes)
}

fn cpp_conditionals(source: &mut String) {
    let (mut depth, mut skip, mut continued) = (0, 0, false);
    let (mut mid_statement, mut block_comment) = (false, false);
    let comment = re!(r"/\*.*?\*/|//[^\n]*");
    let end = re!(r"[;{}]$|^[\w\s]*:$|^[A-Z][A-Z0-9_]*(?:\s*\(.*\))?$");
    for range in lines(source) {
        let line = &source[range.clone()];
        if continued {
            continued = line.trim_end().ends_with('\\');
            continue;
        }
        if let Some(word) = directive(line).filter(|_| !block_comment) {
            continued = line.trim_end().ends_with('\\');
            let opens = matches!(word, "if" | "ifdef" | "ifndef");
            if depth == 0 {
                if opens && mid_statement && !continued {
                    depth = 1;
                    blank_range(source, range);
                }
                continue;
            }
            match word {
                _ if opens => depth += 1,
                "else" | "elif" | "elifdef" | "elifndef" if skip == 0 => skip = depth,
                "endif" => {
                    if skip == depth {
                        skip = 0;
                    }
                    depth -= 1;
                }
                "else" | "elif" | "elifdef" | "elifndef" => (),
                _ => continue,
            }
            blank_range(source, range);
        } else if skip != 0 {
            blank_range(source, range);
        } else if block_comment {
            block_comment = !line.contains("*/");
        } else {
            let code = comment.replace_all(line, "");
            let code = if let Some((before, _)) = code.split_once("/*") {
                block_comment = true;
                before.trim()
            } else {
                code.trim()
            };
            if !code.is_empty() {
                mid_statement = !end.is_match(code);
            }
        }
    }
}

fn cplusplus_guard(source: &mut String) {
    let open = re!(
        r"^[ \t]*#[ \t]*(?:ifdef[ \t]+__cplusplus\b|if[ \t]+defined[ \t]*\(?[ \t]*__cplusplus\b)"
    );
    let ranges = lines(source);
    for (i, header) in ranges.iter().enumerate() {
        if !open.is_match(&source[header.clone()]) {
            continue;
        }
        for range in ranges.iter().skip(i + 1).take(41) {
            if let Some(word) = directive(&source[range.clone()]) {
                if word == "endif" {
                    blank_range(source, header.end..range.start);
                }
                break;
            }
        }
    }
}

fn lone_macros(source: &mut String) {
    let lone = re!(r"^[ \t]*([A-Z][A-Z0-9_]{3,})[ \t]*(?://[^\r\n]*|/\*[^\r\n]*\*/[ \t]*)?\r?\n?$");
    let comment = re!(r"/\*.*?\*/|//[^\n]*");
    let mut prev_continues = false;
    let ranges = lines(source);
    let mut next = 0;
    for (i, range) in ranges.iter().enumerate() {
        let line = &source[range.clone()];
        if line.trim().is_empty() {
            continue;
        }
        next = next.max(i + 1);
        while next < ranges.len() && source[ranges[next].clone()].trim().is_empty() {
            next += 1;
        }
        let next_ok = ranges.get(next).is_none_or(|r| {
            source[r.clone()]
                .trim_start()
                .starts_with(|c: char| c.is_ascii_alphabetic() || "_#{}~".contains(c))
        });
        if let Some(m) = lone.captures(line).and_then(|cap| cap.get(1)) {
            if m.as_str().contains('_') && !prev_continues && next_ok {
                blank_range(source, range.start + m.start()..range.start + m.end());
                continue;
            }
        }
        let code = comment.replace_all(line, "");
        let code = code.trim();
        prev_continues = !code.starts_with('*')
            && !re!(r"^template\s*<").is_match(code)
            && code.ends_with(|c| "=+-*/%&|^<>?:,(\\".contains(c));
    }
}

fn close_paren(source: &str, open: usize, max_lines: usize) -> Option<usize> {
    let mut depth = 0;
    let mut quote = None;
    let mut escaped = false;
    let mut lines = 0;
    for (offset, byte) in source.as_bytes()[open..]
        .iter()
        .copied()
        .take(64 * 1024)
        .enumerate()
    {
        if byte == b'\n' {
            lines += 1;
            if lines > max_lines {
                return None;
            }
        }
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == delimiter {
                quote = None;
            }
            continue;
        }
        match byte {
            b'"' | b'\'' => quote = Some(byte),
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + offset + 1);
                }
            }
            _ => (),
        }
    }
    None
}

fn macro_calls(
    source: &mut String,
    pattern: &Regex,
    max_lines: usize,
    select: impl Fn(&str, Range<usize>, usize) -> Option<Range<usize>>,
) {
    let mut ranges = Vec::<Range<usize>>::new();
    for cap in pattern.captures_iter(source) {
        let (Some(head), Some(name)) = (cap.get(0), cap.get(1)) else {
            continue;
        };
        if ranges.last().is_some_and(|r| r.end > name.start()) {
            continue;
        }
        let Some(end) = close_paren(source, head.end() - 1, max_lines) else {
            continue;
        };
        if let Some(range) = select(source, name.range(), end) {
            ranges.push(range);
        }
    }
    for range in ranges {
        blank_range(source, range);
    }
}

fn cpp_prefixes(source: &mut String) {
    let alias = re!(r"(?m)^[ \t]*#[ \t]*define[ \t]+interface[ \t]+struct\b").is_match(source);
    let declarations = re!(r"(?m)(?:^|[;{}])[ \t]*(interface) [A-Za-z_]\w*\s*(:[^;{}]+)?\{")
        .captures_iter(source)
        .filter_map(|cap| {
            let tail = &source[cap.get(0)?.end()..];
            let body = tail.split('}').next().unwrap_or(tail);
            (alias || cap.get(2).is_some() || re!(r"\bvirtual\b").is_match(body))
                .then(|| cap.get(1).map(|m| m.range()))
                .flatten()
        })
        .collect::<Vec<_>>();
    for range in declarations {
        source.replace_range(range, "struct   ");
    }

    blank_groups(
        source,
        re!(r"\b(?:class|struct)\s+([A-Z][A-Z0-9_]+)\s+[A-Za-z_]\w*(?:\s+final)?\s*[:{]"),
        1,
    );
    let pattern = re!(
        r"\b(FORCEINLINE_DEBUGGABLE|FORCENOINLINE|FORCEINLINE|PUGI__FN_NO_INLINE|PUGI__FN|PUGIXML_FUNCTION|_ALWAYS_INLINE_|_FORCE_INLINE_|BOOST_FORCEINLINE|BOOST_NOINLINE|Q_INVOKABLE|Q_SCRIPTABLE|Q_ALWAYS_INLINE|Q_SLOT|Q_SIGNAL|FOLLY_ALWAYS_INLINE|FOLLY_NOINLINE|ABSL_ATTRIBUTE_ALWAYS_INLINE|ABSL_ATTRIBUTE_NOINLINE|LLVM_ATTRIBUTE_ALWAYS_INLINE|LLVM_ATTRIBUTE_NOINLINE|V8_INLINE|V8_NOINLINE|EIGEN_STRONG_INLINE|EIGEN_ALWAYS_INLINE|EIGEN_DEVICE_FUNC|RAPIDJSON_FORCEINLINE|MOZ_ALWAYS_INLINE|MOZ_NEVER_INLINE|PROTOBUF_ALWAYS_INLINE|PROTOBUF_NOINLINE|FMT_CONSTEXPR20|FMT_CONSTEXPR|FMT_INLINE|JSON_HEDLEY_ALWAYS_INLINE|JSON_HEDLEY_NEVER_INLINE|HEDLEY_ALWAYS_INLINE|HEDLEY_NEVER_INLINE|GLM_FUNC_QUALIFIER|GLM_FUNC_DECL|GLM_CONSTEXPR|GLM_INLINE|SIMD_FORCE_INLINE|SK_ALWAYS_INLINE|CV_ALWAYS_INLINE|CV_INLINE|EA_FORCE_INLINE|EA_NOINLINE|CC_INLINE|NEVER_INLINE|G_INLINE_FUNC|SQLITE_PRIVATE|SQLITE_API|STDMETHODCALLTYPE|WINAPIV|WINAPI|APIENTRY|ALWAYS_INLINE|FORCE_INLINE|NOINLINE|[A-Z][A-Z0-9_]*(?:_API|_EXPORT|_ABI))\b"
    );
    let ranges = pattern
        .find_iter(source)
        .filter(|m| {
            let after = &source[m.end()..];
            after.starts_with(char::is_whitespace)
                && after
                    .trim_start()
                    .starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        })
        .map(|m| m.range())
        .collect::<Vec<_>>();
    for range in ranges {
        blank_range(source, range);
    }
    macro_calls(
        source,
        re!(r"\b(UMETA|UPARAM|UE_DEPRECATED\w*)\s*\("),
        usize::MAX,
        |_, name, end| Some(name.start..end),
    );
}

fn cpp_access(source: &mut String) {
    let pattern = re!(r"(?m)^[ \t]*([A-Z][A-Z0-9]*_[A-Z0-9_]*[ \t]*):[ \t]*(?://[^\r\n]*)?\r?$");
    let replacements = pattern
        .captures_iter(source)
        .filter_map(|cap| {
            let label = cap.get(1)?;
            let name = label.as_str().trim_end();
            let access = if name.contains("PRIVATE") {
                "private"
            } else if name.contains("PROTECTED") {
                "protected"
            } else if name.contains("PUBLIC") || matches!(name, "Q_SIGNALS" | "Q_SLOTS") {
                "public"
            } else {
                return None;
            };
            let padding = label.len().checked_sub(access.len())?;
            Some((label.range(), format!("{access}{}", " ".repeat(padding))))
        })
        .collect::<Vec<_>>();
    for (range, replacement) in replacements {
        source.replace_range(range, &replacement);
    }
}

fn c_annotations(source: &mut String) {
    let bare = re!(
        r"\b(?:__init|__exit|__initdata|__initconst|__exitdata|__devinit|__devexit|__cpuinit|__meminit|__meminitdata|__net_init|__net_exit|__net_initdata|__init_or_module|__user|__kernel|__iomem|__percpu|__rcu|__force|__nocast|__must_check|__maybe_unused|__always_unused|__used|__cold|__hot|__weak|__pure|__sched|__malloc|__visible|__deprecated|__ro_after_init|__read_mostly|__refdata|__latent_entropy|__randomize_layout|__no_randomize_layout|__bpf_kfunc|__function_aligned|__always_inline|__noreturn|__cacheline_aligned_in_smp|__cacheline_aligned|__cacheline_internodealigned_in_smp|____cacheline_aligned_in_smp|____cacheline_aligned|____cacheline_internodealigned_in_smp|__noclone|__lockfunc|__ref|__private|__bitwise|__nosavedata|__no_kcsan|__cpuidle|__ksym|__initdata_memblock|__initdata_or_meminfo)\b"
    );
    let ranges = bare
        .find_iter(source)
        .filter(|m| !source[m.end()..].trim_start().starts_with('('))
        .map(|m| m.range())
        .collect::<Vec<_>>();
    for range in ranges {
        blank_range(source, range);
    }
    let parameterized = re!(
        r"\b(__free|__printf|__scanf|__counted_by|__counted_by_le|__counted_by_be|__guarded_by|__pt_guarded_by|__acquires|__releases|__must_hold|__cleanup|__aligned|__section|__bpf_md_ptr|__assume_aligned)[ \t]*\("
    );
    macro_calls(source, parameterized, 40, |src, name, end| {
        let from = src[..name.start].rfind('\n').map_or(0, |i| i + 1);
        let field =
            src[from..name.start].trim().is_empty() && src[end..].trim_start().starts_with(';');
        let end = if field {
            end + src[end..].find(';')? + 1
        } else {
            end
        };
        Some(name.start..end)
    });
    blank_groups(
        source,
        re!(
            r"\b(?:static|extern|inline)[ \t]+(noinline_for_stack|nokprobe_inline|noinline|notrace|noinstr)\b[ \t]+[A-Za-z_]"
        ),
        1,
    );
    blank_groups(source, re!(r"\b(auto)[ \t]+[A-Za-z_]\w*[ \t]*="), 1);
    blank_groups(
        source,
        re!(r"\b[A-Za-z_]\w*[ \t]+([A-Z][A-Z0-9_]{2,})[ \t]*[,)]"),
        1,
    );
}

fn c_macro_args(source: &mut String) {
    macro_calls(
        source,
        re!(r"(?m)^[ \t]*([A-Z_][A-Z0-9_]*)\s*\("),
        40,
        |src, name, end| {
            let open = name.end + src[name.end..].find('(')? + 1;
            let args = &src[open..end - 1];
            (src[end..].trim_start().starts_with(';')
                && re!(r"(?:^|[,{(\s])(?:\.[A-Za-z_]\w*|\[[^\]]+\])\s*=[^=]").is_match(args))
            .then_some(open..end - 1)
        },
    );

    let excluded = re!(
        r"^(?:if|while|for|switch|sizeof|_Alignof|alignof|typeof|typeof_unqual|__typeof__|_Generic|return|_Static_assert|static_assert|__attribute__|__declspec|__builtin_types_compatible_p)$"
    );
    let type_arg = re!(r"^(struct|union|enum)[ \t\r\n]+[A-Za-z_]\w*[ \t\r\n]*(\**)[ \t\r\n]*[,)]");
    let mut edits = Vec::new();
    for cap in re!(r"\b([A-Za-z_]\w*)[ \t]*\(").captures_iter(source) {
        let (Some(head), Some(name)) = (cap.get(0), cap.get(1)) else {
            continue;
        };
        if excluded.is_match(name.as_str()) {
            continue;
        }
        let preceding = source[..name.start()].trim_end_matches([' ', '\t']);
        if preceding.ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '*')
            && preceding
                .rsplit(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .next()
                != Some("return")
        {
            continue;
        }
        let tail = &source[head.end()..];
        let mut depth = 0;
        let mut arg_start = true;
        let mut quoted = None;
        let mut escaped = false;
        for (offset, byte) in tail.bytes().take(600).enumerate() {
            if let Some(quote) = quoted {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == quote {
                    quoted = None;
                }
                continue;
            }
            match byte {
                b' ' | b'\t' | b'\r' | b'\n' => continue,
                b';' | b'{' | b'}' => break,
                b')' if depth == 0 => break,
                b',' if depth == 0 => {
                    arg_start = true;
                    continue;
                }
                b'(' => depth += 1,
                b')' => depth -= 1,
                b'"' | b'\'' => quoted = Some(byte),
                b's' | b'u' | b'e' if depth == 0 && arg_start => {
                    if let Some(cap) = type_arg.captures(&tail[offset..]) {
                        let start = head.end() + offset;
                        for group in [1, 2] {
                            if let Some(m) = cap.get(group) {
                                edits.push(start + m.start()..start + m.end());
                            }
                        }
                    }
                }
                _ => (),
            }
            arg_start = false;
        }
    }
    for range in edits {
        blank_range(source, range);
    }
    c_va_arg(source);
}

fn c_va_arg(source: &mut String) {
    let spans = re!(r"\bva_arg[ \t]*\(([^(),]+)(,[^()]*)\)")
        .captures_iter(source)
        .filter_map(|cap| cap.get(2))
        .filter(|arg| !re!(r"^,[ \t]*[A-Za-z_]\w*[ \t]*$").is_match(arg.as_str()))
        .map(|arg| arg.range())
        .collect::<Vec<_>>();
    for span in spans {
        blank_range(source, span);
    }
}

fn c_declaration_macros(source: &mut String) {
    macro_calls(
        source,
        re!(r"(?m)(^[ \t]*(?:static|extern)[ \t]+[A-Z][A-Z0-9_]{2,})[ \t]*\("),
        0,
        |src, name, end| {
            let tail = src[end..].split('\n').next()?;
            (tail.trim() == ";").then_some(name.start..end + tail.len())
        },
    );
    let initializers = re!(
        r#"(?m)^([ \t]*)static[ \t]+[A-Z][A-Z0-9_]{2,}[ \t]*\(([^();'"\r\n]*?),[ \t]*([A-Za-z_]\w*)[ \t]*\)([ \t]*=[ \t]*\{?[ \t]*\r?)$"#
    );
    let edits = initializers
        .captures_iter(source)
        .filter_map(|cap| {
            let (indent, ty, name, tail) = (cap.get(1)?, cap.get(2)?, cap.get(3)?, cap.get(4)?);
            if !re!(r"^[A-Za-z_][\w \t*]*$").is_match(ty.as_str().trim()) {
                return None;
            }
            let prefix = format!(
                "{}static {}",
                indent.as_str(),
                ty.as_str().split_whitespace().collect::<Vec<_>>().join(" ")
            );
            let pad = (name.start() - indent.start()).checked_sub(prefix.len() + 1)? + 1;
            let suffix = " ".repeat(tail.start() - name.end());
            Some((
                indent.start()..tail.start(),
                format!(
                    "{prefix}{}{name}{suffix}",
                    " ".repeat(pad),
                    name = name.as_str()
                ),
            ))
        })
        .collect::<Vec<_>>();
    for (range, text) in edits {
        source.replace_range(range, &text);
    }
}

fn c_iterator_macros(source: &mut String) {
    macro_calls(
        source,
        re!(r"(?m)^[ \t]+([a-z_][a-z0-9_]*)[ \t]*\("),
        5,
        |src, name, end| {
            if [
                "if",
                "while",
                "for",
                "switch",
                "return",
                "sizeof",
                "do",
                "else",
                "case",
                "typeof",
                "__typeof__",
                "_Static_assert",
                "static_assert",
            ]
            .contains(&&src[name.clone()])
            {
                return None;
            }
            let call = &src[name.start..end];
            if call.contains('\n') && call.contains([';', '{', '}']) {
                return None;
            }
            let after = &src[end..];
            let same_line = after.split('\n').next()?.trim();
            (same_line == "{"
                || same_line.is_empty()
                    && after
                        .trim_start()
                        .starts_with(|c: char| c.is_ascii_alphabetic() || c == '_' || c == '{'))
            .then_some(name.start..end)
        },
    );
}

fn cuda(source: &mut String) {
    blank_groups(
        source,
        re!(
            r"\b__launch_bounds__\s*\([^()\n]*\)|\b__(?:global|device|host|constant|shared|managed|grid_constant|forceinline|noinline|launch_bounds)__\b"
        ),
        0,
    );
    let ranges = re!(r"<<<[^;]{0,400}?>>>")
        .find_iter(source)
        .filter(|m| {
            let mut braces = 0;
            for b in m.as_str().bytes() {
                if b == b'{' {
                    braces += 1;
                }
                if b == b'}' {
                    braces -= 1;
                }
                if braces < 0 {
                    return false;
                }
            }
            braces == 0
        })
        .map(|m| m.range())
        .collect::<Vec<_>>();
    for range in ranges {
        blank_range(source, range);
    }
}

pub(super) fn prepare(source: &str, path: &str, cpp: bool) -> String {
    let (mut out, raw) = raw_strings(source);
    if cpp {
        cpp_access(&mut out);
        cpp_conditionals(&mut out);
    } else {
        cplusplus_guard(&mut out);
    }
    let before_macros = out.clone();
    if cpp {
        cpp_prefixes(&mut out);
    } else {
        c_annotations(&mut out);
        c_macro_args(&mut out);
        c_declaration_macros(&mut out);
    }
    macro_calls(
        &mut out,
        re!(r"(?m)^[ \t]*([A-Z][A-Z0-9_]{2,})\s*\("),
        usize::MAX,
        |src, name, end| {
            src[end..]
                .trim_start()
                .starts_with(|c: char| c.is_ascii_alphabetic() || "_~#".contains(c))
                .then_some(name.start..end)
        },
    );
    if !cpp {
        c_iterator_macros(&mut out);
    }
    blank_groups(
        &mut out,
        re!(r"(?m)^[ \t]*([A-Z][A-Z0-9_]{2,})\s+[A-Za-z_]\w*[\s*]+[A-Za-z_]\w*\s*\("),
        1,
    );
    lone_macros(&mut out);
    let ext = path.rsplit('.').next().unwrap_or_default();
    if ext.eq_ignore_ascii_case("metal") {
        blank_groups(
            &mut out,
            re!(
                r"\[\[\s*[A-Za-z_]\w*(?:\s*\([^()\n]*\))?(?:\s*,\s*[A-Za-z_]\w*(?:\s*\([^()\n]*\))?)*\s*\]\]"
            ),
            0,
        );
    } else if ext.eq_ignore_ascii_case("cu")
        || ext.eq_ignore_ascii_case("cuh")
        || ["__global__", "__device__", "__constant__", "cudaStream_t"]
            .iter()
            .any(|s| out.contains(s))
    {
        cuda(&mut out);
    }
    let mut continued = false;
    for range in lines(&before_macros) {
        let line = &before_macros[range.clone()];
        let is_directive = continued || directive(line).is_some();
        if is_directive {
            out.replace_range(range, line);
        }
        continued = is_directive && line.trim_end().ends_with('\\');
    }
    if !cpp {
        blank_groups(
            &mut out,
            re!(r"(?m)^[ \t]*#[ \t]*define[ \t]+[A-Za-z_]\w*\([^()\n]*?\b\w+(\.\.\.)"),
            1,
        );
    }
    restore_raw(source, out, raw)
}
