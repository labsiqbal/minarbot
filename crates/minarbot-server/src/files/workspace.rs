use super::*;
use crate::config::WorkspaceConfig;
use crate::operator_file;
use sha2::{Digest, Sha256};
use std::{ffi::OsStr, fs::File, io::Read, os::unix::fs::MetadataExt, path::Path};
fn unavailable() -> Failure {
    error(400,"Cannot read the workspace. Check the path and host permissions, then reload from the start. Only UTF-8 text up to 256 KiB without links is supported.")
}
pub fn open_directory(path: &Path) -> std::io::Result<File> {
    if !path.is_absolute() {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    let mut current = File::open("/")?;
    for part in path
        .as_os_str()
        .as_encoded_bytes()
        .split(|&b| b == b'/')
        .skip(1)
    {
        if part.is_empty() || part == b"." || part == b".." {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        let part = std::str::from_utf8(part).map_err(|_| std::io::ErrorKind::InvalidInput)?;
        current = operator_file::directory_at(&current, part)?;
    }
    Ok(current)
}
pub fn validate_bindings(bindings: &[WorkspaceConfig]) -> crate::db::Result<()> {
    if bindings.len() > 256 {
        return Err("Workspace bindings allow at most 256 entries".into());
    }
    let mut ids = std::collections::HashSet::new();
    for binding in bindings {
        let directory = binding
            .directory
            .to_str()
            .ok_or("Invalid workspace binding")?;
        if !valid_id(&binding.agent)
            || !ids.insert(&binding.agent)
            || directory.encode_utf16().count() > 4096
            || !directory.starts_with('/')
            || directory
                .split('/')
                .skip(1)
                .any(|part| part.is_empty() || part == "." || part == "..")
            || directory == "/"
            || directory
                .chars()
                .any(|c| c <= '\x1f' || c == '\x7f' || c == '\\')
        {
            return Err("Invalid workspace binding".into());
        }
        open_directory(&binding.directory)
            .map_err(|_| "Workspace directory unavailable or contains a symbolic link")?;
    }
    Ok(())
}
fn path(value: &str, empty: bool) -> Result<&str> {
    if (!empty && value.is_empty())
        || value.encode_utf16().count() > 2048
        || value
            .chars()
            .any(|c| c <= '\x1f' || c == '\x7f' || c == '\\')
        || !value.is_empty()
            && value
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(unavailable());
    }
    Ok(value)
}
fn directory(root: &Path, relative: &str, listing: bool) -> Result<(File, String)> {
    let relative = path(relative, listing)?;
    let mut root = root.to_path_buf();
    let name = if listing {
        if !relative.is_empty() {
            root.push(relative);
        }
        String::new()
    } else {
        let (parent, name) = relative.rsplit_once('/').unwrap_or(("", relative));
        if !parent.is_empty() {
            root.push(parent);
        }
        name.to_owned()
    };
    let fd = open_directory(&root).map_err(|_| unavailable())?;
    Ok((fd, name))
}
pub(super) fn read_bytes(directory: &File, name: &str, limit: usize) -> Result<Vec<u8>> {
    let file = operator_file::file_at(directory, OsStr::new(name)).map_err(|_| unavailable())?;
    let meta = file.metadata().map_err(|_| unavailable())?;
    if !meta.is_file() || meta.nlink() != 1 || meta.len() > limit as u64 {
        return Err(unavailable());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable())?;
    if bytes.len() > limit {
        return Err(unavailable());
    }
    Ok(bytes)
}
pub fn download(root: &Path, relative: &str) -> Result<(String, Vec<u8>)> {
    download_bounded(root, relative, 10 * 1024 * 1024)
}
pub(crate) fn download_bounded(
    root: &Path,
    relative: &str,
    limit: usize,
) -> Result<(String, Vec<u8>)> {
    let (fd, name) = directory(root, relative, false)?;
    let bytes = read_bytes(&fd, &name, limit)?;
    Ok((name, bytes))
}
pub fn read(root: &Path, listing: bool, args: &[(String, String)]) -> Result<Value> {
    if !query_keys(
        args,
        if listing {
            &["path", "after"]
        } else {
            &["path", "offset", "digest"]
        },
    ) {
        return Err(unavailable());
    }
    let path = parameter(args, "path").unwrap_or("");
    let after = parameter(args, "after").unwrap_or("");
    let offset = parameter(args, "offset")
        .unwrap_or("0")
        .parse::<usize>()
        .map_err(|_| unavailable())?;
    if after.encode_utf16().count() > 255
        || after.contains('/')
        || after.contains('\0')
        || offset > 9007199254740991
        || parameter(args, "digest").is_some_and(|d| {
            d.len() != 64
                || !d
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    {
        return Err(unavailable());
    }
    let (fd, name) = directory(root, path, listing)?;
    if listing {
        let entries = operator_file::entries(&fd).map_err(|_| unavailable())?;
        let mut rows = Vec::new();
        for (i, entry) in entries.enumerate() {
            if i >= 10000 {
                return Err(unavailable());
            }
            let (name, kind) = entry.map_err(|_| unavailable())?;
            let name = name.to_string_lossy().into_owned();
            if name.encode_utf16().cmp(after.encode_utf16()) != std::cmp::Ordering::Greater {
                continue;
            }
            rows.push(json!({"name":name,"kind":match kind { rustix::fs::FileType::RegularFile => "file", rustix::fs::FileType::Directory => "directory", _ => "unsupported" }}));
        }
        rows.sort_by(|a, b| {
            text(a, "name")
                .encode_utf16()
                .cmp(text(b, "name").encode_utf16())
        });
        let next = if rows.len() > 100 {
            rows[99]["name"].clone()
        } else {
            Value::Null
        };
        rows.truncate(100);
        return Ok(json!({"path":path,"entries":rows,"next_after":next}));
    }
    let bytes = read_bytes(&fd, &name, 256 * 1024)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| unavailable())?
        .trim_start_matches('\u{feff}');
    if text
        .chars()
        .any(|c| c <= '\x08' || c == '\x0b' || c == '\x0c' || ('\x0e'..='\x1f').contains(&c))
    {
        return Err(unavailable());
    }
    let digest = format!("{:x}", Sha256::digest(&bytes));
    if parameter(args, "digest").is_some_and(|d| d != digest) {
        return Err(error(409,"File changed since the previous page. Reload from the start to read the latest version."));
    }
    let length = text.encode_utf16().count();
    Ok(
        json!({"path":path,"digest":digest,"text":text,"next_offset":if offset.saturating_add(8000)<length{Some(offset+8000)}else{None}}),
    )
}
pub(super) fn response(root: &Path, listing: bool, args: &[(String, String)]) -> Result<Response> {
    let value = read(root, listing, args)?;
    if listing {
        return Ok(json_response(200, value));
    }
    let offset = parameter(args, "offset")
        .unwrap_or("0")
        .parse::<usize>()
        .map_err(|_| unavailable())?;
    Ok(crate::data::wire::workspace(&value, offset))
}
pub async fn handle(state: &AppState, req: Request, parts: &[&str]) -> Result<Response> {
    if req.method() != Method::GET {
        return Err(error(404, "Route not found."));
    }
    origin(state, &req)?;
    let agent = parts[2];
    let kind = parts[4];
    if !valid_id(agent) {
        return Err(error(400, "Invalid agent ID."));
    }
    let binding = {
        let db = state.database.lock().map_err(|_| Failure::storage())?;
        crate::data::agents::read(&db.connection, agent)?;
        crate::data::agent_folders::effective(state, &db.connection, agent)?
    };
    let args = query(&req);
    let allowed = if kind == "entries" {
        vec!["path", "after"]
    } else if kind == "download" {
        vec!["path"]
    } else {
        vec!["path", "offset", "digest"]
    };
    if args.iter().any(|(k, _)| !allowed.contains(&k.as_str())) {
        return Err(error(400, "Invalid workspace parameters."));
    }
    if !query_keys(&args, &allowed) {
        return Err(error(400, "Duplicate workspace parameter."));
    }
    if parameter(&args, "offset").is_some_and(|v| {
        v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) || v.len() > 1 && v.starts_with('0')
    }) {
        return Err(error(400, "Invalid text position."));
    }
    if kind == "download" {
        let (name,bytes)=download(&binding.directory,parameter(&args,"path").unwrap_or("")).map_err(|_|error(400,"Cannot download the file. Download limit is 10 MiB; directories, links, and special files are unsupported."))?;
        return Ok(attachments::download(&name, bytes));
    }
    response(&binding.directory, kind == "entries", &args)
}
