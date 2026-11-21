use crate::{
    db::Result,
    files::skills::parse_files,
    operator_file::{self, bounded_read, directory_at, file_at, open_directory, publish},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use rustix::fs::FileType;
use serde_json::{json, Value};
use std::{
    fs::{self, File},
    os::unix::fs::MetadataExt,
    path::Path,
};

const PACKAGE_LIMIT: usize = 2 * 1024 * 1024 + 48_000;
const LIST_LIMIT: usize = 100;

fn entries(directory: &File, limit_error: &str) -> Result<Vec<operator_file::DirectoryEntry>> {
    let mut entries = operator_file::entries(directory)?
        .take(LIST_LIMIT + 1)
        .collect::<std::io::Result<Vec<_>>>()?;
    if entries.len() > LIST_LIMIT {
        return Err(limit_error.to_owned().into());
    }
    entries.sort_by(|a, b| {
        a.0.to_string_lossy()
            .encode_utf16()
            .cmp(b.0.to_string_lossy().encode_utf16())
    });
    Ok(entries)
}

#[derive(Default)]
struct Package {
    files: Vec<Value>,
    entries: usize,
    bytes: usize,
}

impl Package {
    fn scan(&mut self, directory: &File, prefix: &str, depth: usize) -> Result<()> {
        if depth > 16 {
            return Err("Skill folder exceeds 16 levels.".into());
        }
        for (name, kind) in entries(directory, "Skill folder is too large.")? {
            self.entries += 1;
            if self.entries > 128 {
                return Err("Skill folder exceeds 128 entries.".into());
            }
            let name = name.to_str().ok_or("Skill file name must be UTF-8.")?;
            if name.starts_with('.') {
                return Err("Hidden entries are not included automatically. Choose a package folder without hidden entries.".into());
            }
            let path = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            if path.encode_utf16().count() > 240 {
                return Err("Skill file path is too long.".into());
            }
            if kind == FileType::Directory {
                self.scan(&directory_at(directory, name)?, &path, depth + 1)?;
            } else if kind == FileType::RegularFile {
                if self.files.len() >= 32 {
                    return Err("Skill folder allows at most 32 files.".into());
                }
                let file = file_at(directory, name.as_ref())?;
                let remaining = PACKAGE_LIMIT - self.bytes;
                let bytes = bounded_read(file, remaining).map_err(|error| {
                    if error.kind() == std::io::ErrorKind::FileTooLarge {
                        std::io::Error::new(
                            error.kind(),
                            "Skill folder exceeds the package size limit.",
                        )
                    } else {
                        error
                    }
                })?;
                self.bytes += bytes.len();
                let text = std::str::from_utf8(&bytes)
                    .ok()
                    .filter(|text| !text.bytes().any(|b| matches!(b, 0..=8 | 11 | 12 | 14..=31)));
                self.files.push(match text {
                    Some(text) => json!({"path":path,"content":text}),
                    None => {
                        json!({"path":path,"content":STANDARD.encode(bytes),"encoding":"base64"})
                    }
                });
            } else {
                return Err("Links and special files are not supported in skill packages.".into());
            }
        }
        Ok(())
    }
}

fn read_package(directory: &File, name: &str) -> Result<Value> {
    let mut package = Package::default();
    package.scan(directory, "", 0)?;
    let skill = parse_files(&Value::Array(package.files)).map_err(|error| error.message)?;
    if skill["name"] != name {
        return Err("Folder name must match the name in SKILL.md.".into());
    }
    Ok(skill)
}

fn discover(root: &File) -> Result<Value> {
    let mut result = Vec::new();
    for (name, _) in entries(
        root,
        "Discovery is limited to 100 direct entries. Choose a smaller directory.",
    )? {
        let label = name.to_string_lossy();
        if label.starts_with('.') {
            continue;
        }
        let skill = directory_at(root, Path::new(&name))
            .map_err(|_| "Entry is not a regular package directory.".into())
            .and_then(|child| read_package(&child, &label));
        result.push(match skill {
            Ok(skill) => json!({"directory": label,"status":"valid","name":skill["name"],"description":skill["description"],"files":skill["files"].as_array().map(Vec::len)}),
            Err(error) => json!({"directory":label,"status":"invalid","error":error.to_string()}),
        });
    }
    Ok(Value::Array(result))
}

fn outside_source(source: &File, parent: &File) -> Result<bool> {
    let source = source.metadata()?;
    let mut ancestor = parent.try_clone()?;
    loop {
        let current = ancestor.metadata()?;
        if (source.dev(), source.ino()) == (current.dev(), current.ino()) {
            return Ok(false);
        }
        ancestor = directory_at(&ancestor, "..")?;
        let next = ancestor.metadata()?;
        if (next.dev(), next.ino()) == (current.dev(), current.ino()) {
            return Ok(true);
        }
    }
}

pub fn run(args: &[String]) -> Result<Value> {
    match args {
        [command, directory] if command == "list" => {
            discover(&open_directory(&fs::canonicalize(directory)?)?)
        }
        [command, directory, output] if command == "export" => {
            let source = fs::canonicalize(directory)?;
            let root = open_directory(&source)?;
            let skill = read_package(
                &root,
                source
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(""),
            )?;
            let output = Path::new(output);
            let parent = fs::canonicalize(
                output
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new(".")),
            )?;
            let name = output.file_name().ok_or("Invalid export file name.")?;
            let output = parent.join(name);
            let target = open_directory(&parent)?;
            if output == source || output.starts_with(&source) || !outside_source(&root, &target)? {
                return Err("Save the export outside the source folder.".into());
            }
            let bundle =
                json!({"format":"minarbot-skill-folder","version":1,"files":skill["files"]});
            publish(&target, name, &serde_json::to_vec(&bundle)?)?;
            Ok(
                json!({"name":skill["name"],"files":skill["files"].as_array().map(Vec::len),"output":output}),
            )
        }
        _ => Err("Use: skills list DIRECTORY or skills export SKILL_FOLDER NEW_JSON_FILE.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_refuses_child_symlink_after_listing() -> Result<()> {
        let root =
            std::env::temp_dir().join(format!("minarbot-skill-swap-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root)?;
        let result = (|| -> Result<()> {
            let library = root.join("library");
            let package = library.join("sample");
            let external = root.join("external");
            fs::create_dir_all(&package)?;
            fs::create_dir(&external)?;
            let pinned = open_directory(&library)?;
            let children = entries(&pinned, "Too many entries.")?;
            fs::rename(&package, root.join("original"))?;
            std::os::unix::fs::symlink(&external, &package)?;
            assert!(directory_at(&pinned, Path::new(&children[0].0)).is_err());
            assert_eq!(discover(&pinned)?[0]["status"], "invalid");
            Ok(())
        })();
        fs::remove_dir_all(root)?;
        result
    }
}
