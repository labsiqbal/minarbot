use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};

fn keys(value: &Value, keys: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|value| value.keys().all(|key| keys.contains(&key.as_str())))
}
fn text<'a>(value: &'a Value, field: &str) -> &'a str {
    value[field].as_str().unwrap_or_default()
}
pub fn parse_files(value: &Value) -> Result<Value, String> {
    let files = value
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 32 && value.to_string().len() <= 4 * 1024 * 1024)
        .ok_or_else(|| {
            "Skill folder limit is 32 files, 48 KB of text, and 2 MiB of binary assets.".to_owned()
        })?;
    let mut names = std::collections::HashSet::new();
    let mut result = Vec::new();
    let mut binary = 0;
    for file in files {
        let path = text(file, "path");
        let content = file.get("content").and_then(Value::as_str);
        if !keys(file, &["path", "content", "encoding"])
            || path.encode_utf16().count() > 240
            || path
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
            || path
                .chars()
                .any(|c| c == '\\' || c == ':' || c <= '\x1f' || c == '\x7f')
            || content.is_none()
            || file.get("encoding").is_some_and(|v| v != "base64")
            || !names.insert(path.to_lowercase())
        {
            return Err(
                "Invalid skill file path or content. Use UTF-8 text files without duplicate paths."
                    .to_owned(),
            );
        }
        let content = content.ok_or("Invalid skill file content.")?;
        let encoded = file.get("encoding").is_some();
        if encoded {
            if path == "SKILL.md" || content.len() > 2 * 1024 * 1024_usize.div_ceil(3) * 4 {
                return Err(
                    "Binary assets must use standard base64; SKILL.md must be UTF-8 text."
                        .to_owned(),
                );
            }
            let bytes = STANDARD.decode(content).map_err(|_| {
                "Binary assets are invalid or their total exceeds 2 MiB.".to_owned()
            })?;
            binary += bytes.len();
            if binary > 2 * 1024 * 1024 || STANDARD.encode(bytes) != content {
                return Err("Binary assets are invalid or their total exceeds 2 MiB.".to_owned());
            }
        } else if content
            .chars()
            .any(|c| c <= '\x08' || c == '\x0b' || c == '\x0c' || ('\x0e'..='\x1f').contains(&c))
        {
            return Err(
                "Invalid skill file path or content. Use UTF-8 text files without duplicate paths."
                    .to_owned(),
            );
        }
        let mut item = json!({"path":path,"content":content});
        if encoded {
            item["encoding"] = json!("base64");
        }
        result.push(item);
    }
    let text_files: Vec<_> = result
        .iter()
        .filter(|v| v.get("encoding").is_none())
        .collect();
    if serde_json::to_vec(&text_files)
        .map_err(|_| "Skill files could not be serialized.")?
        .len()
        > 48000
    {
        return Err("Skill text file limit is 48 KB.".to_owned());
    }
    for file in &result {
        let parts: Vec<_> = text(file, "path").split('/').collect();
        for n in 1..parts.len() {
            if names.contains(&parts[..n].join("/").to_lowercase()) {
                return Err("File path conflicts with a folder. Rename the file or folder before importing.".to_owned());
            }
        }
    }
    let source = result
        .iter()
        .find(|v| v["path"] == "SKILL.md")
        .map(|v| text(v, "content"))
        .unwrap_or("")
        .trim_start_matches('\u{feff}');
    let source = source
        .strip_prefix("---\r\n")
        .or_else(|| source.strip_prefix("---\n"))
        .ok_or_else(|| "SKILL.md must have YAML frontmatter.".to_owned())?;
    let mut end = None;
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            end = Some((offset, offset + line.len()));
            break;
        }
        offset += line.len();
    }
    let (end, start) = end.ok_or_else(|| "SKILL.md must have YAML frontmatter.".to_owned())?;
    let yaml = &source[..end];
    if yaml
        .lines()
        .any(|l| l.contains(" &") || l.contains(" *") || l.contains("!!"))
    {
        return Err("Invalid SKILL.md frontmatter.".to_owned());
    }
    let meta: Value =
        serde_yaml::from_str(yaml).map_err(|_| "Invalid SKILL.md frontmatter.".to_owned())?;
    let content = source[start..].trim();
    let name = text(&meta, "name");
    let description = text(&meta, "description");
    if !meta.is_object()
        || name.is_empty()
        || name.len() > 64
        || name.split('-').any(|p| {
            p.is_empty()
                || !p
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
        || description.trim().is_empty()
        || description.encode_utf16().count() > 1024
        || content.is_empty()
        || content.encode_utf16().count() > 16000
    {
        return Err("SKILL.md requires a slug name, a description up to 1024 characters, and instructions up to 16000 characters.".to_owned());
    }
    if !keys(
        &meta,
        &[
            "name",
            "description",
            "license",
            "compatibility",
            "metadata",
        ],
    ) {
        return Err("Skill uses unsupported tool, activation, or provider rules. Import cancelled so these rules are not ignored.".to_owned());
    }
    if meta.get("license").is_some_and(|v| !v.is_string())
        || meta.get("compatibility").is_some_and(|v| {
            !v.as_str()
                .is_some_and(|s| !s.trim().is_empty() && s.encode_utf16().count() <= 500)
        })
        || meta.get("metadata").is_some_and(|v| {
            !v.as_object()
                .is_some_and(|m| m.values().all(Value::is_string))
        })
    {
        return Err(
            "License and compatibility must be text; metadata must contain name and text pairs."
                .to_owned(),
        );
    }
    Ok(json!({"name":name,"description":description.trim(),"content":content,"files":result}))
}
