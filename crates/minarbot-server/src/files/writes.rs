use super::*;
use crate::operator_file;
use rustix::fs::{self as at, AtFlags, FileType, Mode, OFlags};
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::{fs, path::PathBuf};
use std::{fs::File, io::Write, path::Path};

fn invalid() -> Failure {
    error(400, "Invalid workspace path")
}
fn path(value: &str, max: usize) -> Result<&str> {
    if value.is_empty()
        || value.encode_utf16().count() > max
        || value
            .chars()
            .any(|c| c <= '\x1f' || c == '\x7f' || c == '\\')
        || value
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(invalid());
    }
    Ok(value)
}
fn parent(root: &Path, relative: &str, max: usize) -> Result<(File, String)> {
    path(relative, max)?;
    let (parent, name) = relative.rsplit_once('/').unwrap_or(("", relative));
    Ok((
        workspace::open_directory(&if parent.is_empty() {
            root.to_path_buf()
        } else {
            root.join(parent)
        })
        .map_err(|_| invalid())?,
        name.to_owned(),
    ))
}
#[cfg(all(test, target_os = "linux"))]
fn location(fd: &File, name: &str) -> PathBuf {
    use std::os::fd::AsRawFd;
    PathBuf::from(format!("/proc/self/fd/{}/{name}", fd.as_raw_fd()))
}
fn io<T>(value: std::io::Result<T>) -> Result<T> {
    value.map_err(|_| error(400, "Workspace operation failed."))
}
fn digest(args: &Value) -> Result<&str> {
    args["digest"]
        .as_str()
        .filter(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .ok_or_else(|| error(400, "Invalid workspace deletion"))
}
fn text_file(args: &Value) -> Result<&str> {
    args["text"]
        .as_str()
        .filter(|s| {
            s.len() <= 32768
                && s.encode_utf16().count() <= 32768
                && !s.chars().any(|c| {
                    c <= '\x08' || c == '\x0b' || c == '\x0c' || ('\x0e'..='\x1f').contains(&c)
                })
        })
        .ok_or_else(|| error(400, "Invalid workspace file"))
}
fn publish(root: &Path, relative: &str, bytes: &[u8]) -> Result<Value> {
    let (fd, name) = parent(root, relative, 2048)?;
    let staged = format!(".minarbot-{}.tmp", uuid::Uuid::new_v4());
    let mut output = File::from(
        at::openat(
            &fd,
            staged.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|_| error(400, "Workspace operation failed."))?,
    );
    let result = (|| {
        io(output.write_all(bytes))?;
        io(output.sync_all())?;
        #[cfg(all(test, target_os = "linux"))]
        tests::fault(
            "before-link",
            &location(&fd, &staged),
            &location(&fd, &name),
        );
        io(
            at::linkat(&fd, staged.as_str(), &fd, name.as_str(), AtFlags::empty())
                .map_err(Into::into),
        )?;
        #[cfg(all(test, target_os = "linux"))]
        tests::fault("after-link", &location(&fd, &staged), &location(&fd, &name));
        Ok(
            json!({"path":relative,"digest":format!("{:x}",Sha256::digest(bytes)),"size":bytes.len()}),
        )
    })();
    drop(output);
    let cleanup = io(at::unlinkat(&fd, staged.as_str(), AtFlags::empty()).map_err(Into::into))
        .and_then(|_| io(fd.sync_all()));
    match result {
        Err(e) => Err(e),
        Ok(v) => {
            cleanup?;
            Ok(v)
        }
    }
}
pub fn publish_generated(
    binding: &crate::config::WorkspaceConfig,
    name: &str,
    text: &str,
    previous: Option<&str>,
) -> Result<()> {
    if !binding.write {
        return Err(error(403, "Workspace does not have write permission."));
    }
    let root = &binding.directory;
    let (fd, filename) = parent(root, name, 2048)?;
    match at::statat(&fd, filename.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
        Err(rustix::io::Errno::NOENT) => {
            publish(root, name, text.as_bytes())?;
        }
        Ok(meta) if FileType::from_raw_mode(meta.st_mode) == FileType::RegularFile => {
            let bytes = workspace::read_bytes(&fd, &filename, 32768)?;
            if bytes == text.as_bytes() {
                return Ok(());
            }
            let digest = format!("{:x}", Sha256::digest(&bytes));
            if previous != Some(digest.as_str()) {
                return Err(error(409, "File already exists. Read it before editing."));
            }
            let replaced = perform(
                root,
                "replace_workspace_file",
                &json!({"path":name,"text":text,"digest":digest}),
            )?;
            if replaced["replaced"] != true {
                return Err(error(409, text_result_error(&replaced)));
            }
        }
        _ => {
            return Err(error(
                409,
                "Workspace destination is unavailable or is not a regular file.",
            ))
        }
    }
    Ok(())
}

fn text_result_error(result: &Value) -> String {
    let message = text(result, "error");
    let recovery = text(result, "recoveryPath");
    format!("{message} Recovery file: {recovery}")
}

fn remove(root: &Path, args: &Value) -> Result<Value> {
    if !keys(args, &["path", "digest"]) {
        return Err(error(400, "Invalid workspace deletion"));
    }
    let relative = text(args, "path");
    let expected = digest(args)?;
    let (fd, name) = parent(root, relative, 2048)?;
    let bytes = workspace::read_bytes(&fd, &name, 10 * 1024 * 1024)?;
    if format!("{:x}", Sha256::digest(bytes)) != expected {
        return Err(error(
            409,
            "File changed since it was read. Removal cancelled.",
        ));
    }
    let recovery = format!(".minarbot-removed-{}", uuid::Uuid::new_v4());
    let recovery_path = relative
        .rsplit_once('/')
        .map(|(p, _)| format!("{p}/{recovery}/file"))
        .unwrap_or_else(|| format!("{recovery}/file"));
    io(at::mkdirat(&fd, recovery.as_str(), Mode::RWXU).map_err(Into::into))?;
    let saved = io(operator_file::directory_at(&fd, &recovery))?;
    io(fd.sync_all())?;
    let moved = (|| {
        #[cfg(all(test, target_os = "linux"))]
        tests::fault(
            "before-rename",
            &location(&fd, &name),
            &location(&saved, "file"),
        );
        io(at::renameat(&fd, name.as_str(), &saved, "file").map_err(Into::into))?;
        io(saved.sync_all())?;
        io(fd.sync_all())?;
        let bytes = workspace::read_bytes(&saved, "file", 10 * 1024 * 1024)?;
        if format!("{:x}", Sha256::digest(bytes)) != expected {
            return Err(error(409, "File changed during removal"));
        }
        Ok(json!({"path":relative,"removed":true,"digest":expected,"recoveryPath":recovery_path}))
    })();
    Ok(moved.unwrap_or_else(|_:Failure|json!({"path":relative,"uncertain":true,"recoveryPath":recovery_path,"error":"Removal did not confirm success. Inspect the original and recovery paths before retrying. Recovery content was not erased."})))
}
fn restore(root: &Path, args: &Value) -> Result<Value> {
    if !keys(args, &["path", "recoveryPath", "digest"]) {
        return Err(error(400, "Invalid workspace recovery"));
    }
    path(text(args, "path"), 2048)?;
    let source = path(text(args, "recoveryPath"), 4096)?;
    let parts: Vec<_> = source.split('/').collect();
    if parts.last() != Some(&"file")
        || !parts
            .get(parts.len().saturating_sub(2))
            .and_then(|directory| directory.strip_prefix(".minarbot-removed-"))
            .is_some_and(valid_id)
    {
        return Err(error(400, "Invalid workspace recovery path"));
    }
    let (fd, name) = parent(root, source, 4096)?;
    let bytes = workspace::read_bytes(&fd, &name, 10 * 1024 * 1024)?;
    if format!("{:x}", Sha256::digest(&bytes)) != digest(args)? {
        return Err(error(409, "Recovery file changed; restoration refused"));
    }
    let mut result = publish(root, text(args, "path"), &bytes)?;
    result["restored"] = json!(true);
    result["recoveryPath"] = json!(source);
    Ok(result)
}
fn perform(root: &Path, name: &str, args: &Value) -> Result<Value> {
    match name {
        "create_workspace_file" => {
            if !keys(args, &["path", "text"]) {
                return Err(error(400, "Invalid workspace file"));
            }
            publish(root, text(args, "path"), text_file(args)?.as_bytes())
        }
        "create_workspace_directory" => {
            if !keys(args, &["path"]) {
                return Err(error(400, "Invalid workspace directory"));
            }
            let (fd, name) = parent(root, text(args, "path"), 2048)?;
            io(at::mkdirat(&fd, name.as_str(), Mode::RWXU).map_err(Into::into))?;
            io(fd.sync_all())?;
            Ok(json!({"path":args["path"],"kind":"directory"}))
        }
        "delete_workspace_file" => remove(root, args),
        "restore_workspace_file" => restore(root, args),
        "replace_workspace_file" => {
            if !keys(args, &["path", "digest", "text"]) {
                return Err(error(400, "Invalid workspace replacement"));
            }
            let content = text_file(args)?;
            path(text(args, "path"), 2048)?;
            digest(args)?;
            let removed = remove(root, &json!({"path":args["path"],"digest":args["digest"]}))?;
            if removed["removed"] != true {
                return Ok(removed);
            }
            Ok(
                match publish(root, text(args, "path"), content.as_bytes()) {
                    Ok(mut result) => {
                        result["replaced"] = json!(true);
                        result["previousDigest"] = args["digest"].clone();
                        result["recoveryPath"] = removed["recoveryPath"].clone();
                        result
                    }
                    Err(_) => {
                        json!({"path":args["path"],"uncertain":true,"recoveryPath":removed["recoveryPath"],"error":"Replacement did not confirm publication. Inspect the original path and recovery bytes before retrying."})
                    }
                },
            )
        }
        _ => Err(error(400, "Workspace tool is unavailable.")),
    }
}

fn skill_copy(db: &Connection, root: &Path, agent: &str, args: &Value) -> Result<Vec<u8>> {
    if !keys(args, &["id", "source", "path"])
        || args["id"].as_str().is_none_or(|v| v.len() > 36)
        || args["source"]
            .as_str()
            .is_none_or(|v| v.encode_utf16().count() > 240)
    {
        return Err(error(400, "Invalid skill file copy"));
    }
    parent(root, text(args, "path"), 2048)?;
    let row=one(db,"SELECT f.files FROM agent_skills a JOIN skill_files f ON f.skill=a.skill AND f.revision=a.version WHERE a.agent=? AND a.skill=? AND a.enabled=1 AND a.removed=0",[agent,text(args,"id")])?.ok_or_else(||error(404,"Enabled installed skill file not found"))?;
    let files: Value = serde_json::from_str(text(&row, "files")).map_err(|_| Failure::storage())?;
    let parsed = skills::parse_files(&files)?;
    let file = parsed["files"]
        .as_array()
        .and_then(|files| files.iter().find(|f| f["path"] == args["source"]))
        .ok_or_else(|| error(404, "Enabled installed skill file not found"))?;
    let bytes = if file["encoding"] == "base64" {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(text(file, "content"))
            .map_err(|_| Failure::storage())?
    } else {
        text(file, "content").as_bytes().to_vec()
    };
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(error(400, "Invalid workspace asset"));
    }
    Ok(bytes)
}

pub fn initialize(db: &Connection) -> crate::db::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS flow_tool_calls(seq INTEGER PRIMARY KEY AUTOINCREMENT,turn TEXT NOT NULL REFERENCES turns(id),call_id TEXT NOT NULL,call TEXT NOT NULL,result TEXT NOT NULL,after_output INTEGER NOT NULL,after_action INTEGER,UNIQUE(turn,call_id)) STRICT;")?;
    let columns =
        crate::files::rows(db, "PRAGMA table_info(flow_tool_calls)", []).map_err(|e| e.message)?;
    if !columns.iter().any(|row| row["name"] == "after_action") {
        db.execute(
            "ALTER TABLE flow_tool_calls ADD COLUMN after_action INTEGER",
            [],
        )?;
    }
    db.execute_batch("CREATE TABLE IF NOT EXISTS workspace_writes(turn TEXT NOT NULL REFERENCES turns(id),call_id TEXT NOT NULL,call TEXT NOT NULL,after_output INTEGER NOT NULL,after_action INTEGER NOT NULL,PRIMARY KEY(turn,call_id)) STRICT;
        INSERT INTO flow_tool_calls(turn,call_id,call,result,after_output,after_action)
        SELECT turn,call_id,call,'{\"uncertain\":true,\"error\":\"Workspace operation was interrupted. Ask the host owner to inspect the requested path, sibling .minarbot-removed-* recovery directories and orphan .minarbot-*.tmp staging links before any new write; no automatic replay occurred.\"}',after_output,after_action
        FROM workspace_writes w WHERE NOT EXISTS (SELECT 1 FROM flow_tool_calls f WHERE f.turn=w.turn AND f.call_id=w.call_id);")?;
    Ok(())
}
pub fn execute(
    db: &Connection,
    binding: &crate::config::WorkspaceConfig,
    turn: &str,
    call: &Value,
    after_action: i64,
) -> Result<Value> {
    if !binding.write {
        return Err(error(403, "Workspace does not have write permission."));
    }
    let row = one(db, "SELECT agent,status FROM turns WHERE id=?", [turn])?
        .ok_or_else(|| error(409, "Task has stopped."))?;
    if row["agent"] != binding.agent || row["status"] != "running" {
        return Err(error(409, "Task has stopped."));
    }
    let call_id = text(call, "id");
    if call_id.is_empty() || call_id.encode_utf16().count() > 200 {
        return Err(error(400, "Invalid tool call ID."));
    }
    if one(
        db,
        "SELECT 1 FROM workspace_writes WHERE turn=? AND call_id=?",
        [turn, call_id],
    )?
    .is_some()
    {
        return Err(error(409, "Workspace write already attempted"));
    }
    let args: Value = serde_json::from_str(text(&call["function"], "arguments"))
        .map_err(|_| error(400, "Invalid JSON."))?;
    let name = text(&call["function"], "name");
    if !args.is_object() {
        return Err(error(400, "Invalid workspace arguments."));
    }
    let asset = if name == "copy_skill_file" {
        Some(skill_copy(db, &binding.directory, &binding.agent, &args)?)
    } else {
        None
    };
    let count: i64 = db.query_row(
        "SELECT count(*) FROM generated_files WHERE turn=?",
        [turn],
        |r| r.get(0),
    )?;
    db.execute(
        "INSERT INTO workspace_writes VALUES(?,?,?,?,?)",
        rusqlite::params![turn, call_id, call.to_string(), count, after_action],
    )?;
    #[cfg(test)]
    tests::fault("after-intent", &binding.directory, &binding.directory);
    let effect = match asset {
        Some(bytes) => publish(&binding.directory, text(&args, "path"), &bytes),
        None => perform(&binding.directory, name, &args),
    };
    #[cfg(test)]
    tests::fault("after-effect", &binding.directory, &binding.directory);
    let result=effect.unwrap_or_else(|_|json!({"uncertain":true,"error":if name=="delete_workspace_file" {"Removal did not confirm success. Inspect the original path and sibling .minarbot-removed-* recovery directories before retrying."}else{"Workspace operation did not confirm success. Existing files are never replaced. Ask the host owner to inspect the requested path and orphan .minarbot-*.tmp staging links before trying again."}}));
    db.execute("INSERT INTO flow_tool_calls(turn,call_id,call,result,after_output,after_action) VALUES(?,?,?,?,?,?)",rusqlite::params![turn,call_id,call.to_string(),result.to_string(),count,after_action])?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::{fs::MetadataExt, process::ExitStatusExt};

    thread_local! {
        static FAULT: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
    }

    pub(super) fn fault(phase: &str, source: &Path, destination: &Path) {
        FAULT.with(|fault| match (fault.borrow().as_str(), phase) {
            ("race-file", "before-rename") => {
                fs::write(source, b"REPLACEMENT").unwrap_or_else(|e| panic!("{e}"))
            }
            ("race-directory", "before-rename") => {
                fs::remove_file(source).unwrap_or_else(|e| panic!("{e}"));
                fs::create_dir(source).unwrap_or_else(|e| panic!("{e}"));
                fs::write(source.join("kept"), b"KEPT").unwrap_or_else(|e| panic!("{e}"));
            }
            ("race-link", "before-rename") => {
                fs::remove_file(source).unwrap_or_else(|e| panic!("{e}"));
                std::os::unix::fs::symlink("../outside", source).unwrap_or_else(|e| panic!("{e}"));
            }
            ("race-publication", "before-link") => {
                fs::write(destination, b"COMPETING").unwrap_or_else(|e| panic!("{e}"))
            }
            (point, stage) if point == stage => rustix::process::kill_process(
                rustix::process::getpid(),
                rustix::process::Signal::KILL,
            )
            .unwrap_or_else(|e| panic!("{e}")),
            _ => {}
        });
    }

    fn set_fault(value: &str) {
        FAULT.with(|fault| *fault.borrow_mut() = value.into());
    }

    fn call(id: &str, name: &str, args: Value) -> Value {
        json!({"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}})
    }

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> crate::db::Result<Self> {
            let root = std::env::temp_dir().join(format!("bs-writes-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&root)?;
            fs::create_dir(root.join("workspace"))?;
            Ok(Self(root))
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn binding(root: &Path, agent: &str) -> crate::config::WorkspaceConfig {
        crate::config::WorkspaceConfig {
            agent: agent.into(),
            directory: root.join("workspace"),
            write: true,
            managed: false,
        }
    }

    #[test]
    fn killed_workspace_writes_keep_publication_state_and_never_replay() -> crate::db::Result<()> {
        if let Some(root) = std::env::var_os("MINARBOT_TEST_WRITES_CRASH") {
            let root = PathBuf::from(root);
            let command = serde_json::from_str(&std::env::var("MINARBOT_TEST_WRITES_CALL")?)?;
            set_fault(&std::env::var("MINARBOT_TEST_WRITES_PHASE")?);
            let db = Connection::open(root.join("state.sqlite"))?;
            let result = execute(&db, &binding(&root, "agent"), "turn", &command, 0);
            panic!("crash hook not reached: {result:?}");
        }
        let scratch = Scratch::new()?;
        let binding = binding(&scratch.0, "agent");
        let db = Connection::open(scratch.0.join("state.sqlite"))?;
        db.execute_batch("CREATE TABLE turns(id TEXT PRIMARY KEY,agent TEXT,status TEXT);
            INSERT INTO turns VALUES ('turn','agent','running'); CREATE TABLE generated_files(turn TEXT);")?;
        initialize(&db)?;
        for (id, tool, phase, present) in [
            (
                "mkdir-before",
                "create_workspace_directory",
                "after-intent",
                false,
            ),
            (
                "mkdir-after",
                "create_workspace_directory",
                "after-effect",
                true,
            ),
            (
                "file-before",
                "create_workspace_file",
                "after-intent",
                false,
            ),
            ("file-after", "create_workspace_file", "after-effect", true),
            ("linked", "create_workspace_file", "after-link", true),
            ("replace", "replace_workspace_file", "before-link", false),
        ] {
            let path = binding.directory.join(id);
            let args = if tool == "create_workspace_directory" {
                json!({"path":id})
            } else if id == "replace" {
                fs::write(&path, b"OLD")?;
                json!({"path":id,"text":"pending","digest":format!("{:x}",Sha256::digest(b"OLD"))})
            } else {
                json!({"path":id,"text":"pending"})
            };
            let command = call(id, tool, args.clone());
            let child = std::process::Command::new(std::env::current_exe()?)
                .args(["--exact", "files::writes::tests::killed_workspace_writes_keep_publication_state_and_never_replay"])
                .env("MINARBOT_TEST_WRITES_CRASH", &scratch.0)
                .env("MINARBOT_TEST_WRITES_CALL", command.to_string())
                .env("MINARBOT_TEST_WRITES_PHASE", phase)
                .output()?;
            assert_eq!(child.status.signal(), Some(9), "{child:?}");
            initialize(&db)?;
            initialize(&db)?;
            assert_eq!(path.exists(), present);
            let (count, receipt): (i64, String) = db.query_row(
                "SELECT count(*),result FROM flow_tool_calls WHERE call_id=?",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(count, 1);
            assert_eq!(serde_json::from_str::<Value>(&receipt)?["uncertain"], true);
            assert!(receipt.contains("host owner"));
            assert!(execute(&db, &binding, "turn", &command, 0)
                .is_err_and(|e| e.message.contains("already attempted")));
            if id == "linked" {
                let target = fs::metadata(&path)?;
                assert_eq!(target.nlink(), 2);
                assert!(workspace::download(&binding.directory, id).is_err());
                let stage = fs::read_dir(&binding.directory)?
                    .filter_map(std::result::Result::ok)
                    .find(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
                    .ok_or("Staged hard link missing")?;
                assert_eq!(fs::metadata(stage.path())?.ino(), target.ino());
                fs::remove_file(stage.path())?;
                assert_eq!(
                    workspace::download(&binding.directory, id)
                        .map_err(|e| e.message)?
                        .1,
                    b"pending"
                );
            }
            if id == "replace" {
                assert!(!path.exists());
                let recovery = fs::read_dir(&binding.directory)?
                    .filter_map(std::result::Result::ok)
                    .find(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with(".minarbot-removed-")
                    })
                    .ok_or("Recovery directory missing")?;
                let recovery_path = format!("{}/file", recovery.file_name().to_string_lossy());
                assert_eq!(fs::read(binding.directory.join(&recovery_path))?, b"OLD");
                restore(
                    &binding.directory,
                    &json!({"path":id,"digest":args["digest"],"recoveryPath":recovery_path}),
                )
                .map_err(|e| e.message)?;
                assert_eq!(fs::read(&path)?, b"OLD");
            }
        }
        Ok(())
    }

    #[test]
    fn restores_workspace_file_from_legacy_recovery_directory() -> crate::db::Result<()> {
        let scratch = Scratch::new()?;
        let root = scratch.0.join("workspace");
        let directory = format!(".minarbot-removed-{}", uuid::Uuid::new_v4());
        let source = format!("{directory}/file");
        fs::create_dir(root.join(directory))?;
        fs::write(root.join(&source), b"KEEP")?;
        restore(
            &root,
            &json!({"path":"restored","recoveryPath":source,"digest":format!("{:x}", Sha256::digest(b"KEEP"))}),
        )
        .map_err(|error| error.message)?;
        assert_eq!(fs::read(root.join("restored"))?, b"KEEP");
        assert_eq!(fs::read(root.join(source))?, b"KEEP");
        Ok(())
    }

    #[test]
    fn concurrent_workspace_changes_preserve_both_publication_and_recovery_bytes(
    ) -> crate::db::Result<()> {
        let scratch = Scratch::new()?;
        let root = scratch.0.join("workspace");
        fs::write(root.join("outside"), b"OUTSIDE")?;
        let digest = format!("{:x}", Sha256::digest(b"ORIGINAL"));
        let args = json!({"path":"file","digest":digest});
        for (kind, path) in [
            ("race-file", "file"),
            ("race-directory", "file"),
            ("race-link", "file"),
        ] {
            fs::write(root.join(path), b"ORIGINAL")?;
            set_fault(kind);
            let saved = remove(&root, &args);
            set_fault("");
            let saved = saved.map_err(|e| e.message)?;
            assert_eq!(saved["uncertain"], true);
            assert!(saved.get("removed").is_none());
            let recovery = root.join(text(&saved, "recoveryPath"));
            match kind {
                "race-file" => assert_eq!(fs::read(&recovery)?, b"REPLACEMENT"),
                "race-directory" => assert_eq!(fs::read(recovery.join("kept"))?, b"KEPT"),
                _ => assert!(fs::symlink_metadata(&recovery)?.is_symlink()),
            }
            assert_eq!(
                fs::metadata(recovery.parent().ok_or("Missing recovery parent")?)?.mode() & 0o777,
                0o700
            );
            assert_eq!(fs::read(root.join("outside"))?, b"OUTSIDE");
        }

        fs::create_dir(root.join("nested"))?;
        let bytes = vec![255; 10 * 1024 * 1024];
        fs::write(root.join("nested/original"), &bytes)?;
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let removed = remove(&root, &json!({"path":"nested/original","digest":digest}))
            .map_err(|e| e.message)?;
        let source = text(&removed, "recoveryPath");
        let args = json!({"path":"nested/restored","digest":digest,"recoveryPath":source});
        let restored = restore(&root, &args).map_err(|e| e.message)?;
        assert_eq!(restored["size"], bytes.len());
        let path = root.join("nested/restored");
        assert_eq!(fs::read(&path)?, bytes);
        assert_eq!(fs::metadata(&path)?.mode() & 0o777, 0o600);
        assert!(restore(&root, &args).is_err());
        for (key, value) in [
            ("path", json!("../outside")),
            ("recoveryPath", json!("nested/restored")),
            ("digest", json!("0".repeat(64))),
            ("extra", json!(true)),
        ] {
            let mut invalid = args.clone();
            invalid[key] = value;
            assert!(restore(&root, &invalid).is_err());
        }
        std::os::unix::fs::symlink(&path, root.join("nested/link"))?;
        let mut linked = args.clone();
        linked["path"] = json!("nested/link");
        assert!(restore(&root, &linked).is_err());
        let mut raced = args.clone();
        raced["path"] = json!("raced");
        set_fault("race-publication");
        let error = restore(&root, &raced);
        set_fault("");
        assert!(error.is_err());
        assert_eq!(fs::read(root.join("raced"))?, b"COMPETING");
        assert_eq!(fs::read(root.join(source))?, bytes);

        fs::write(root.join("original"), b"OLD")?;
        let digest = format!("{:x}", Sha256::digest(b"OLD"));
        let mut args = json!({"path":"original","digest":digest,"text":"NEW"});
        for bad in [json!("\u{0}"), json!("x".repeat(32769))] {
            args["text"] = bad;
            assert!(perform(&root, "replace_workspace_file", &args).is_err());
            assert_eq!(fs::read(root.join("original"))?, b"OLD");
        }
        args["text"] = json!("NEW");
        set_fault("race-publication");
        let replaced = perform(&root, "replace_workspace_file", &args);
        set_fault("");
        let replaced = replaced.map_err(|e| e.message)?;
        assert_eq!(replaced["uncertain"], true);
        assert!(replaced.get("replaced").is_none());
        assert_eq!(fs::read(root.join("original"))?, b"COMPETING");
        assert_eq!(
            fs::read(root.join(text(&replaced, "recoveryPath")))?,
            b"OLD"
        );
        fs::remove_file(root.join(source))?;
        std::os::unix::fs::symlink(&path, root.join(source))?;
        assert!(restore(
            &root,
            &json!({"path":"link-copy","digest":args["digest"],"recoveryPath":source})
        )
        .is_err());
        assert!(!root.join("link-copy").exists());
        Ok(())
    }

    #[test]
    fn skill_copy_checks_scope_and_source_before_intent_and_recovers_result_loss(
    ) -> crate::db::Result<()> {
        let scratch = Scratch::new()?;
        let binding = binding(&scratch.0, "owner");
        let db = Connection::open_in_memory()?;
        db.execute_batch("CREATE TABLE turns(id TEXT PRIMARY KEY,agent TEXT,status TEXT);
            INSERT INTO turns VALUES ('turn','owner','running'),('foreign','other','running');
            CREATE TABLE generated_files(turn TEXT);
            CREATE TABLE agent_skills(agent TEXT,skill TEXT,version INTEGER,enabled INTEGER,removed INTEGER);
            CREATE TABLE skill_files(skill TEXT,revision INTEGER,files TEXT);
            INSERT INTO agent_skills VALUES ('owner','skill',1,1,0);")?;
        let files = json!([
            {"path":"SKILL.md","content":"---\nname: copy\ndescription: Fixture\n---\nUse assets."},
            {"path":"raw.bin","content":"AP+A","encoding":"base64"}
        ]);
        db.execute(
            "INSERT INTO skill_files VALUES ('skill',1,?)",
            [files.to_string()],
        )?;
        initialize(&db)?;
        let args = json!({"id":"skill","source":"raw.bin","path":"copy.bin"});
        let command = call("copy", "copy_skill_file", args.clone());
        let foreign = crate::config::WorkspaceConfig {
            agent: "other".into(),
            ..binding.clone()
        };
        assert!(execute(&db, &foreign, "foreign", &command, 0).is_err());
        for (key, value) in [
            ("source", json!("missing")),
            ("path", json!("../escape")),
            ("extra", json!(true)),
            ("id", json!(4)),
        ] {
            let mut bad = args.clone();
            bad[key] = value;
            assert!(execute(
                &db,
                &binding,
                "turn",
                &call("copy", "copy_skill_file", bad),
                0
            )
            .is_err());
        }
        db.execute("UPDATE agent_skills SET removed=1", [])?;
        assert!(execute(&db, &binding, "turn", &command, 0).is_err());
        assert_eq!(
            db.query_row("SELECT count(*) FROM workspace_writes", [], |r| r
                .get::<_, i64>(0))?,
            0
        );
        assert!(fs::read_dir(&binding.directory)?.next().is_none());
        db.execute("UPDATE agent_skills SET removed=0", [])?;
        db.execute_batch("CREATE TRIGGER lost_result BEFORE INSERT ON flow_tool_calls BEGIN SELECT RAISE(ABORT,'lost result'); END")?;
        assert!(execute(&db, &binding, "turn", &command, 0).is_err());
        let copy = binding.directory.join("copy.bin");
        assert_eq!(fs::read(&copy)?, [0, 255, 128]);
        db.execute_batch("DROP TRIGGER lost_result")?;
        initialize(&db)?;
        let result: String =
            db.query_row("SELECT result FROM flow_tool_calls", [], |r| r.get(0))?;
        assert_eq!(serde_json::from_str::<Value>(&result)?["uncertain"], true);
        fs::write(&copy, b"OWNER EDIT")?;
        assert!(execute(&db, &binding, "turn", &command, 0)
            .is_err_and(|e| e.message.contains("already attempted")));
        assert_eq!(fs::read(copy)?, b"OWNER EDIT");
        Ok(())
    }

    #[test]
    fn removal_and_restore_result_loss_preserve_recovery_and_never_replay() -> crate::db::Result<()>
    {
        use rusqlite::OptionalExtension;
        for operation in ["delete_workspace_file", "restore_workspace_file"] {
            let root =
                std::env::temp_dir().join(format!("minarbot-rust-writes-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&root)?;
            let result = (|| -> crate::db::Result<()> {
                let db = Connection::open_in_memory()?;
                db.execute_batch(
                    "CREATE TABLE turns(id TEXT PRIMARY KEY,agent TEXT,status TEXT);
                    INSERT INTO turns VALUES ('turn','agent','running');
                    CREATE TABLE generated_files(turn TEXT);",
                )?;
                initialize(&db)?;
                let binding = crate::config::WorkspaceConfig {
                    agent: "agent".into(),
                    directory: root.clone(),
                    write: true,
                    managed: false,
                };
                let path = root.join("file");
                fs::write(&path, b"KEEP")?;
                let mut args =
                    json!({"path":"file","digest":format!("{:x}", Sha256::digest(b"KEEP"))});
                if operation == "restore_workspace_file" {
                    let removed = remove(&root, &args).map_err(|e| e.message)?;
                    assert_eq!(removed["removed"], true);
                    args["recoveryPath"] = removed["recoveryPath"].clone();
                }
                let call = json!({"id":"fixture","type":"function","function":{"name":operation,"arguments":args.to_string()}});
                db.execute_batch(
                    "CREATE TRIGGER lose_result BEFORE INSERT ON flow_tool_calls
                    BEGIN SELECT RAISE(ABORT,'lost result'); END;",
                )?;
                assert!(execute(&db, &binding, "turn", &call, 0).is_err());
                assert_eq!(path.exists(), operation == "restore_workspace_file");
                if path.exists() {
                    assert_eq!(fs::read(&path)?, b"KEEP");
                }
                let recovery = fs::read_dir(&root)?
                    .filter_map(|entry| entry.ok())
                    .find(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with(".minarbot-removed-")
                    })
                    .ok_or("Recovery directory missing")?
                    .path()
                    .join("file");
                assert_eq!(fs::read(&recovery)?, b"KEEP");
                assert_eq!(
                    db.query_row("SELECT result FROM flow_tool_calls", [], |row| row
                        .get::<_, String>(0))
                        .optional()?,
                    None
                );
                fs::write(&path, b"NEW")?;
                db.execute_batch("DROP TRIGGER lose_result")?;
                initialize(&db)?;
                initialize(&db)?;
                let receipt: String = db.query_row(
                    "SELECT result FROM flow_tool_calls WHERE call_id='fixture'",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(serde_json::from_str::<Value>(&receipt)?["uncertain"], true);
                assert_eq!(
                    db.query_row("SELECT count(*) FROM flow_tool_calls", [], |row| row
                        .get::<_, i64>(0))?,
                    1
                );
                let retry = execute(&db, &binding, "turn", &call, 0);
                assert!(retry.is_err_and(|e| e.message.contains("already attempted")));
                assert_eq!(fs::read(&path)?, b"NEW");
                assert_eq!(fs::read(recovery)?, b"KEEP");
                Ok(())
            })();
            fs::remove_dir_all(root)?;
            result?;
        }
        Ok(())
    }

    #[test]
    fn writes_preserve_recovery_bytes_and_never_overwrite_or_follow_links() -> crate::db::Result<()>
    {
        let root =
            std::env::temp_dir().join(format!("minarbot-rust-writes-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root)?;
        let run = |name, args| perform(&root, name, &args).map_err(|e| e.message);
        let result = (|| -> crate::db::Result<()> {
            assert_eq!(
                run("create_workspace_directory", json!({"path":"new"}))?,
                json!({"path":"new","kind":"directory"})
            );
            assert_eq!(fs::metadata(root.join("new"))?.mode() & 0o777, 0o700);
            run("create_workspace_directory", json!({"path":"new/nested"}))?;
            run(
                "create_workspace_file",
                json!({"path":"new/nested/kept","text":"KEEP"}),
            )?;
            run(
                "create_workspace_file",
                json!({"path":"limit","text":"x".repeat(32768)}),
            )?;
            assert_eq!(fs::metadata(root.join("limit"))?.len(), 32768);
            assert!(run(
                "create_workspace_file",
                json!({"path":"over-limit","text":"x".repeat(32769)})
            )
            .is_err());
            let created = run(
                "create_workspace_file",
                json!({"path":"note","text":"original"}),
            )?;
            assert!(run(
                "create_workspace_file",
                json!({"path":"note","text":"overwrite"})
            )
            .is_err());
            assert!(run(
                "delete_workspace_file",
                json!({"path":"note","digest":"0".repeat(64)})
            )
            .is_err());
            let replaced = run(
                "replace_workspace_file",
                json!({"path":"note","digest":created["digest"],"text":"next"}),
            )?;
            assert_eq!(
                fs::read(root.join(text(&replaced, "recoveryPath")))?,
                b"original"
            );
            let restored = run(
                "restore_workspace_file",
                json!({"path":"copy","digest":created["digest"],"recoveryPath":replaced["recoveryPath"]}),
            )?;
            assert_eq!(restored["restored"], true);
            assert_eq!(fs::read(root.join("copy"))?, b"original");
            assert!(run("restore_workspace_file",json!({"path":"copy","digest":created["digest"],"recoveryPath":replaced["recoveryPath"]})).is_err());
            for p in ["../escape", "/tmp/escape", "note/../escape", "note/"] {
                assert!(run("create_workspace_file", json!({"path":p,"text":"bad"})).is_err());
            }
            std::os::unix::fs::symlink("copy", root.join("link"))?;
            for path in [
                "new",
                "note",
                "link",
                "link/escape",
                "../escape",
                "/tmp/escape",
                "missing/child",
                "new/../escape",
                "new//child",
                "",
            ] {
                assert!(
                    run("create_workspace_directory", json!({"path":path})).is_err(),
                    "{path}"
                );
            }
            assert!(run(
                "create_workspace_directory",
                json!({"path":"extra","text":"no"})
            )
            .is_err());
            assert_eq!(fs::read(root.join("new/nested/kept"))?, b"KEEP");
            assert!(run(
                "delete_workspace_file",
                json!({"path":"link","digest":created["digest"]})
            )
            .is_err());
            fs::hard_link(root.join("copy"), root.join("hard"))?;
            assert!(run(
                "delete_workspace_file",
                json!({"path":"hard","digest":created["digest"]})
            )
            .is_err());
            Ok(())
        })();
        fs::remove_dir_all(root)?;
        result
    }
}
