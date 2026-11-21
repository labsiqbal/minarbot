use super::*;
use rustix::fs::{self as at, AtFlags, Mode, OFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
};

// ponytail: serialize Git writes globally; use per-common-directory locks if concurrent repositories need throughput.
static WRITES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "Minarbot"),
    ("GIT_AUTHOR_EMAIL", "checkpoints@minarbot.local"),
    ("GIT_COMMITTER_NAME", "Minarbot"),
    ("GIT_COMMITTER_EMAIL", "checkpoints@minarbot.local"),
];

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Entry {
    path: String,
    mode: String,
    hash: String,
}
struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct IndexLock(PathBuf);
impl Drop for IndexLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn io<T>(result: std::io::Result<T>) -> Result<T> {
    result.map_err(|_| error(400, "Git operation failed or exceeded its output limit."))
}
fn db<T>(state: &AppState, work: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    work(
        &state
            .database
            .lock()
            .map_err(|_| Failure::storage())?
            .connection,
    )
}
fn hash(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}
fn sorted(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut values: Vec<_> = values
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    values.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    values
}
fn only(value: &Value, names: &[&str]) -> Result<()> {
    if keys(value, names) {
        Ok(())
    } else {
        Err(error(400, "Invalid Git arguments."))
    }
}
fn check_path(root: &Path, value: &str) -> Result<()> {
    path(value)?;
    let mut current = root.to_owned();
    for part in value.split('/') {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(m) if m.is_dir() || m.is_file() && m.nlink() == 1 => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            _ => {
                return Err(error(
                    400,
                    "Links and special files are unavailable in source control.",
                ))
            }
        }
    }
    Ok(())
}
async fn command(root: &Path, a: &[String], env: &[(&str, &str)], input: &[u8]) -> Result<Vec<u8>> {
    Ok(run_input(root, a, &[], 512 * 1024, false, env, input)
        .await?
        .0)
}
async fn string(root: &Path, a: &[String], env: &[(&str, &str)], input: &[u8]) -> Result<String> {
    Ok(String::from_utf8(command(root, a, env, input).await?)
        .map_err(|_| error(400, "Git output is not UTF-8."))?
        .trim_end()
        .to_owned())
}
fn paths(bytes: &[u8]) -> Result<Vec<String>> {
    let source = std::str::from_utf8(bytes).map_err(|_| error(400, "Git output is not UTF-8."))?;
    let values = source
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(|s| path(s).map(str::to_owned))
        .collect::<Result<Vec<_>>>()?;
    if values.len() > 1000 {
        return Err(error(
            400,
            "Workspace exceeds 1000 files. Bind a smaller workspace.",
        ));
    }
    Ok(sorted(values))
}
async fn names(repo: &Repo, untracked: bool) -> Result<Vec<String>> {
    let mut a = args(&["ls-files", "--cached"]);
    if untracked {
        a.extend(args(&["--others", "--exclude-standard"]));
    }
    a.extend(args(&["-z", "--", "."]));
    let mut values = paths(&command(&repo.directory, &a, &[], &[]).await?)?;
    if let Some(head) = &repo.head {
        for file in paths(
            &command(
                &repo.directory,
                &args(&[
                    "ls-tree",
                    "-r",
                    "-z",
                    "--full-name",
                    "--name-only",
                    head,
                    "--",
                    ".",
                ]),
                &[],
                &[],
            )
            .await?,
        )? {
            values.push(if repo.prefix.is_empty() {
                file
            } else {
                file.strip_prefix(&format!("{}/", repo.prefix))
                    .ok_or_else(|| error(400, "Workspace is outside its repository."))?
                    .to_owned()
            });
        }
    }
    let values = sorted(values);
    if values.len() > 1000 {
        return Err(error(
            400,
            "Workspace exceeds 1000 files. Bind a smaller workspace.",
        ));
    }
    Ok(values)
}
async fn snapshot(repo: &Repo, selected: &[String], write: bool) -> Result<Vec<Entry>> {
    if selected.len() > 1000 {
        return Err(error(
            400,
            "Workspace exceeds 1000 files. Bind a smaller workspace.",
        ));
    }
    let mut entries = Vec::new();
    let mut total = 0;
    for file in selected {
        check_path(&repo.directory, file)?;
        let meta = match fs::symlink_metadata(repo.directory.join(file)) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                return Err(error(
                    400,
                    "Git operation failed or exceeded its output limit.",
                ))
            }
        };
        if !meta.is_file() {
            return Err(error(
                400,
                "Directories, submodules and links cannot be captured. Choose regular files.",
            ));
        }
        let (_, bytes) = workspace::download(&repo.directory, file)?;
        total += bytes.len();
        if total > 32 * 1024 * 1024 {
            return Err(error(
                400,
                "Workspace capture exceeds 32 MiB. Bind a smaller workspace.",
            ));
        }
        let mut a = args(&["hash-object", "--no-filters"]);
        if write {
            a.push("-w".to_owned());
        }
        a.push("--stdin".to_owned());
        entries.push(Entry {
            path: file.clone(),
            mode: if meta.mode() & 0o111 != 0 {
                "100755"
            } else {
                "100644"
            }
            .to_owned(),
            hash: string(&repo.directory, &a, &[], &bytes).await?,
        });
    }
    Ok(entries)
}
async fn tree(repo: &Repo, reference: &str) -> Result<Vec<Entry>> {
    let bytes = command(
        &repo.root,
        &args(&["ls-tree", "-r", "--full-tree", "-z", reference]),
        &[],
        &[],
    )
    .await?;
    let source = std::str::from_utf8(&bytes).map_err(|_| error(400, "Git output is not UTF-8."))?;
    let mut entries = Vec::new();
    for row in source.split('\0').filter(|s| !s.is_empty()) {
        let (meta, file) = row
            .split_once('\t')
            .ok_or_else(|| error(400, "Checkpoint contains unsupported entries."))?;
        let fields: Vec<_> = meta.split(' ').collect();
        if fields.len() != 3
            || !["100644", "100755"].contains(&fields[0])
            || fields[1] != "blob"
            || ![40, 64].contains(&fields[2].len())
            || !fields[2]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(error(400, "Checkpoint contains unsupported entries."));
        }
        entries.push(Entry {
            path: path(file)?.to_owned(),
            mode: fields[0].to_owned(),
            hash: fields[2].to_owned(),
        });
    }
    Ok(entries)
}
fn temporary(repo: &Repo) -> Result<Temporary> {
    let root = repo
        .common
        .join(format!("minarbot-index-{}", uuid::Uuid::new_v4()));
    io(fs::DirBuilder::new().mode(0o700).create(&root))?;
    Ok(Temporary(root))
}
async fn index_info(
    repo: &Repo,
    index: &Path,
    entries: &[Entry],
    removed: &[String],
) -> Result<()> {
    let zero = "0".repeat(
        repo.head
            .as_ref()
            .map(String::len)
            .or_else(|| entries.first().map(|e| e.hash.len()))
            .unwrap_or(40),
    );
    let mut input = String::new();
    for file in removed {
        input.push_str(&format!("0 {zero}\t{file}\0"));
    }
    for entry in entries {
        input.push_str(&format!("{} {}\t{}\0", entry.mode, entry.hash, entry.path));
    }
    if !input.is_empty() {
        command(
            &repo.root,
            &args(&["update-index", "-z", "--index-info"]),
            &[(
                "GIT_INDEX_FILE",
                index.to_str().ok_or_else(Failure::storage)?,
            )],
            input.as_bytes(),
        )
        .await?;
    }
    Ok(())
}
pub fn initialize(db: &Connection) -> crate::db::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS workspace_git_operations(turn TEXT NOT NULL,call_id TEXT NOT NULL,request TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN('prepared','done','uncertain')),result TEXT,call TEXT,after_output INTEGER NOT NULL,after_action INTEGER NOT NULL,PRIMARY KEY(turn,call_id)) STRICT;
    UPDATE workspace_git_operations SET state='uncertain',result='{\"uncertain\":true,\"error\":\"Git operation was interrupted. Inspect Git status and checkpoints; this call will never be replayed.\"}' WHERE state='prepared';
    INSERT OR IGNORE INTO flow_tool_calls(turn,call_id,call,result,after_output,after_action) SELECT turn,call_id,call,result,after_output,after_action FROM workspace_git_operations WHERE call IS NOT NULL AND result IS NOT NULL;
    CREATE TABLE IF NOT EXISTS workspace_checkpoints(turn TEXT NOT NULL,agent TEXT NOT NULL,directory TEXT NOT NULL,ref TEXT NOT NULL,hash TEXT,state TEXT NOT NULL CHECK(state IN('prepared','done','uncertain','pruned')),created INTEGER NOT NULL,PRIMARY KEY(agent,turn)) STRICT;
    UPDATE workspace_checkpoints SET state='uncertain' WHERE state='prepared';
    CREATE TABLE IF NOT EXISTS agent_delete_checkpoints(agent TEXT NOT NULL,directory TEXT NOT NULL,ref TEXT NOT NULL,hash TEXT NOT NULL,PRIMARY KEY(agent,directory,ref)) STRICT;")?;
    Ok(())
}
async fn repository(binding: &WorkspaceConfig, write: bool) -> Result<Repo> {
    if write && !binding.write {
        return Err(error(403, "Workspace does not have write permission."));
    }
    detect(binding)
        .await?
        .ok_or_else(|| error(404, "Workspace is not a Git repository."))
}
async fn capture_repo(
    state: &AppState,
    repo: &Repo,
    agent: &str,
    turn: &str,
    include: &[String],
) -> Result<Value> {
    if !valid_id(turn) {
        return Err(error(400, "Invalid checkpoint ID."));
    }
    if let Some(old) = db(state, |db| {
        one(
            db,
            "SELECT * FROM workspace_checkpoints WHERE agent=? AND turn=?",
            [agent, turn],
        )
    })? {
        if old["directory"] != repo.directory.to_string_lossy().as_ref() || old["state"] != "done" {
            return Err(error(409,"Checkpoint is unavailable or uncertain. Inspect the workspace before starting a new turn."));
        }
        return Ok(old);
    }
    let reference = format!("refs/minarbot/checkpoints/{turn}");
    db(state, |db| {
        db.execute(
            "INSERT INTO workspace_checkpoints VALUES(?,?,?,?,NULL,'prepared',?)",
            rusqlite::params![
                turn,
                agent,
                repo.directory.to_str(),
                reference,
                state.config.now()
            ],
        )?;
        Ok(())
    })?;
    let result=async {
        let selected=sorted(names(repo,true).await?.into_iter().chain(include.iter().cloned()));
        let entries=snapshot(repo,&selected,true).await?; let temp=temporary(repo)?; let index=temp.0.join("index");
        let env=[("GIT_INDEX_FILE",index.to_str().ok_or_else(Failure::storage)?)];
        command(&repo.directory,&args(&["read-tree","--empty"]),&env,&[]).await?;
        index_info(repo,&index,&entries,&[]).await?;
        let tree=string(&repo.directory,&args(&["write-tree"]),&env,&[]).await?;
        let commit=string(&repo.directory,&args(&["commit-tree",&tree,"-m",&format!("Minarbot checkpoint {turn}")]),&IDENTITY,&[]).await?;
        command(&repo.directory,&args(&["update-ref",&reference,&commit,&"0".repeat(commit.len())]),&[],&[]).await?;
        db(state,|db|{db.execute("UPDATE workspace_checkpoints SET state='done',hash=? WHERE agent=? AND turn=?",[&commit,agent,turn])?;Ok(())})?;
        let stale=db(state,|db|rows(db,"SELECT * FROM workspace_checkpoints WHERE directory=? AND state='done' ORDER BY created DESC,rowid DESC LIMIT -1 OFFSET 50",[repo.directory.to_str()]))?;
        for row in stale {
            command(&repo.directory,&args(&["update-ref","-d",text(&row,"ref"),text(&row,"hash")]),&[],&[]).await?;
            db(state,|db|{db.execute("UPDATE workspace_checkpoints SET state='pruned' WHERE agent=? AND turn=?",[text(&row,"agent"),text(&row,"turn")])?;Ok(())})?;
        }
        db(state,|db|one(db,"SELECT * FROM workspace_checkpoints WHERE agent=? AND turn=?",[agent,turn]))?.ok_or_else(Failure::storage)
    }.await;
    if result.is_err() {
        db(state, |db| {
            db.execute("UPDATE workspace_checkpoints SET state='uncertain' WHERE agent=? AND turn=? AND state='prepared'",[agent,turn])?;
            Ok(())
        })?;
    }
    result
}
pub async fn capture(
    state: &AppState,
    binding: &WorkspaceConfig,
    turn: &str,
) -> Result<Option<Value>> {
    if !binding.write {
        return Err(error(403, "Workspace does not have write permission."));
    }
    let Some(repo) = detect(binding).await? else {
        return Ok(None);
    };
    let effect = crate::computer::hub::before_effect(
        state,
        turn,
        "tool",
        json!({"kind":"workspace.checkpoint"}),
    )
    .await?;
    let result = crate::computer::hub::run_effect(&effect, async {
        let _lock = WRITES.lock().await;
        capture_repo(state, &repo, &binding.agent, turn, &[]).await
    })
    .await;
    crate::computer::hub::finish_effect(state, effect, "unknown").await?;
    result.map(Some)
}
struct Plan {
    before: Vec<Entry>,
    files: Vec<Value>,
    digest: String,
}
async fn plan(state: &AppState, repo: &Repo, agent: &str, turn: &str) -> Result<Plan> {
    if !valid_id(turn) {
        return Err(error(400, "Invalid checkpoint ID."));
    }
    let saved = db(state, |db| {
        one(
            db,
            "SELECT * FROM workspace_checkpoints WHERE agent=? AND turn=?",
            [agent, turn],
        )
    })?
    .filter(|s| {
        s["directory"] == repo.directory.to_string_lossy().as_ref()
            && s["state"] == "done"
            && s["hash"].is_string()
    })
    .ok_or_else(|| error(404, "Checkpoint is no longer available."))?;
    if output(
        &repo.directory,
        &["rev-parse", "--verify", "--quiet", text(&saved, "ref")],
        &[1],
    )
    .await?
        != text(&saved, "hash")
    {
        return Err(error(409, "Checkpoint ref changed or was pruned."));
    }
    let before = tree(repo, text(&saved, "hash")).await?;
    let selected = sorted(
        names(repo, true)
            .await?
            .into_iter()
            .chain(before.iter().map(|e| e.path.clone())),
    );
    let current = snapshot(repo, &selected, false).await?;
    let files=selected.into_iter().filter_map(|file| {
        let left=before.iter().find(|e|e.path==file); let right=current.iter().find(|e|e.path==file);
        (left!=right).then(||json!({"path":file,"action":if left.is_none(){"remove"}else if right.is_none(){"restore"}else{"replace"}}))
    }).collect();
    let digest = hash(&json!([
        repo.directory,
        repo.head,
        saved["hash"],
        before
            .iter()
            .map(|entry| json!({"mode":entry.mode,"hash":entry.hash,"path":entry.path}))
            .collect::<Vec<_>>(),
        current
    ]));
    Ok(Plan {
        before,
        files,
        digest,
    })
}
pub async fn preview(state: &AppState, binding: &WorkspaceConfig, turn: &str) -> Result<Value> {
    let _lock = WRITES.lock().await;
    let repo = repository(binding, true).await?;
    let plan = plan(state, &repo, &binding.agent, turn).await?;
    Ok(json!({"turn":turn,"files":plan.files,"digest":plan.digest}))
}
fn begin(
    state: &AppState,
    turn: &str,
    id: &str,
    request: &Value,
    call: Option<&Value>,
    after_action: i64,
) -> Result<Option<Value>> {
    db(state, |db| {
        let encoded = request.to_string();
        if let Some(old) = one(
            db,
            "SELECT request,result FROM workspace_git_operations WHERE turn=? AND call_id=?",
            [turn, id],
        )? {
            if old["request"] != encoded {
                return Err(error(
                    409,
                    "Git call ID was already used for another operation.",
                ));
            }
            return Ok(Some(if old["result"].is_string() {
                serde_json::from_str(text(&old, "result")).map_err(|_| Failure::storage())?
            } else {
                json!({"uncertain":true,"error":"Git operation is still in progress or uncertain. This call will not be replayed."})
            }));
        }
        let count: i64 = db.query_row(
            "SELECT count(*) FROM generated_files WHERE turn=?",
            [turn],
            |r| r.get(0),
        )?;
        db.execute(
            "INSERT INTO workspace_git_operations VALUES(?,?,?,'prepared',NULL,?,?,?)",
            rusqlite::params![
                turn,
                id,
                encoded,
                call.map(Value::to_string),
                count,
                after_action
            ],
        )?;
        Ok(None)
    })
}
fn finish(state: &AppState, turn: &str, id: &str, result: Result<Value>) -> Result<Value> {
    let uncertain = result.is_err();
    let value=result.unwrap_or_else(|e|json!({"uncertain":true,"error":format!("{} Inspect repository state before a new write; this call will never be replayed.",e.message)}));
    db(state, |db| {
        db.execute(
            "UPDATE workspace_git_operations SET state=?,result=? WHERE turn=? AND call_id=?",
            [
                if uncertain { "uncertain" } else { "done" },
                &value.to_string(),
                turn,
                id,
            ],
        )?;
        db.execute("INSERT OR IGNORE INTO flow_tool_calls(turn,call_id,call,result,after_output,after_action) SELECT turn,call_id,call,result,after_output,after_action FROM workspace_git_operations WHERE turn=? AND call_id=? AND call IS NOT NULL",[turn,id])?;
        Ok(value)
    })
}
async fn commit(repo: &Repo, value: &Value) -> Result<Value> {
    only(value, &["message", "paths"])?;
    let message = text(value, "message");
    if message.trim().is_empty()
        || message.encode_utf16().count() > 2000
        || message.chars().any(|c| {
            c <= '\x08'
                || c == '\x0b'
                || c == '\x0c'
                || ('\x0e'..='\x1f').contains(&c)
                || c == '\x7f'
        })
    {
        return Err(error(
            400,
            "Commit message must contain 1..2000 text characters.",
        ));
    }
    let selected = if let Some(v) = value.get("paths") {
        let files = v
            .as_array()
            .filter(|v| !v.is_empty() && v.len() <= 100)
            .ok_or_else(|| error(400, "Choose 1..100 regular-file paths."))?;
        let mut selected = Vec::new();
        for file in files {
            let file = path(file.as_str().unwrap_or(""))?.to_owned();
            if !selected.contains(&file) {
                selected.push(file);
            }
        }
        selected
    } else {
        names(repo, false).await?
    };
    for file in &selected {
        check_path(&repo.directory, file)?;
    }
    if output(
        &repo.directory,
        &["config", "--bool", "--get", "core.sparseCheckout"],
        &[1],
    )
    .await?
        == "true"
        || !command(
            &repo.directory,
            &args(&["ls-files", "--unmerged", "-z"]),
            &[],
            &[],
        )
        .await?
        .is_empty()
    {
        return Err(error(
            400,
            "Sparse checkouts and unmerged index entries are unavailable for commits.",
        ));
    }
    let parent = repo.index.parent().ok_or_else(Failure::storage)?;
    if ["MERGE_HEAD", "rebase-merge", "rebase-apply"]
        .iter()
        .any(|p| parent.join(p).exists())
    {
        return Err(error(400, "Finish the merge or rebase before committing."));
    }
    let lock_path = repo.index.with_file_name(format!(
        "{}.lock",
        repo.index
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(Failure::storage)?
    ));
    let _file = io(OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(&lock_path))?;
    let _lock = IndexLock(lock_path);
    let temp = temporary(repo)?;
    let index = temp.0.join("index");
    let env = [(
        "GIT_INDEX_FILE",
        index.to_str().ok_or_else(Failure::storage)?,
    )];
    command(
        &repo.directory,
        &args(&["read-tree", repo.head.as_deref().unwrap_or("--empty")]),
        &env,
        &[],
    )
    .await?;
    let mut entries = snapshot(repo, &selected, true).await?;
    let full = |file: &str| {
        if repo.prefix.is_empty() {
            file.to_owned()
        } else {
            format!("{}/{file}", repo.prefix)
        }
    };
    for entry in &mut entries {
        entry.path = full(&entry.path);
    }
    let removed: Vec<_> = selected.iter().map(|p| full(p)).collect();
    index_info(repo, &index, &entries, &removed).await?;
    let tree = string(&repo.directory, &args(&["write-tree"]), &env, &[]).await?;
    let original = if let Some(head) = &repo.head {
        output(
            &repo.directory,
            &["rev-parse", &format!("{head}^{{tree}}")],
            &[],
        )
        .await?
    } else {
        string(
            &repo.directory,
            &args(&["hash-object", "-t", "tree", "--stdin"]),
            &[],
            &[],
        )
        .await?
    };
    if tree == original {
        return Err(error(400, "Nothing to commit."));
    }
    let branch = output(&repo.directory, &["symbolic-ref", "--quiet", "HEAD"], &[1]).await?;
    let mut a = args(&["commit-tree", &tree]);
    if let Some(head) = &repo.head {
        a.extend(args(&["-p", head]));
    }
    a.extend(args(&["-m", message]));
    let next = string(&repo.directory, &a, &[], &[]).await?;
    let updated = temp.0.join("updated");
    if repo.index.exists() {
        check_path(parent, "index")?;
        io(fs::copy(&repo.index, &updated))?;
    } else {
        command(
            &repo.directory,
            &args(&["read-tree", "--empty"]),
            &[(
                "GIT_INDEX_FILE",
                updated.to_str().ok_or_else(Failure::storage)?,
            )],
            &[],
        )
        .await?;
    }
    index_info(repo, &updated, &entries, &removed).await?;
    if output(&repo.directory, &["symbolic-ref", "--quiet", "HEAD"], &[1]).await? != branch {
        return Err(error(400, "Current branch changed during commit."));
    }
    let mut a = args(&["update-ref"]);
    if branch.is_empty() {
        a.push("--no-deref".to_owned());
    }
    a.extend(args(&[
        if branch.is_empty() { "HEAD" } else { &branch },
        &next,
        repo.head.as_deref().unwrap_or(&"0".repeat(next.len())),
    ]));
    #[cfg(test)]
    tests::crash_ref_publication(&repo.directory, "before");
    command(&repo.directory, &a, &[], &[]).await?;
    #[cfg(test)]
    tests::crash_ref_publication(&repo.directory, "after");
    io(io(OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(&updated))?
    .sync_all())?;
    io(fs::rename(updated, &repo.index))?;
    io(io(workspace::open_directory(parent))?.sync_all())?;
    Ok(json!({"committed":true,"hash":next,"paths":selected}))
}
fn publish(root: &Path, file: &str, bytes: &[u8], mode: &str, replace: bool) -> Result<()> {
    path(file)?;
    let (parent, name) = file.rsplit_once('/').unwrap_or(("", file));
    let mut directory = root.to_owned();
    if !parent.is_empty() {
        for part in parent.split('/') {
            let fd = io(workspace::open_directory(&directory))?;
            match at::mkdirat(&fd, part, Mode::RWXU) {
                Ok(()) => io(fd.sync_all())?,
                Err(rustix::io::Errno::EXIST) => (),
                Err(e) => return Err(error(400, e.to_string())),
            }
            directory.push(part);
        }
    }
    let fd = io(workspace::open_directory(&directory))?;
    let staged = format!(".minarbot-git-{}.tmp", uuid::Uuid::new_v4());
    let result = (|| {
        let permissions = if mode == "100755" {
            Mode::RWXU
        } else {
            Mode::RUSR | Mode::WUSR
        };
        let mut output = std::fs::File::from(io(at::openat(
            &fd,
            staged.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            permissions,
        )
        .map_err(Into::into))?);
        io(output.write_all(bytes))?;
        io(output.sync_all())?;
        if replace {
            io(at::renameat(&fd, staged.as_str(), &fd, name).map_err(Into::into))?;
        } else {
            io(at::linkat(&fd, staged.as_str(), &fd, name, AtFlags::empty()).map_err(Into::into))?;
        }
        io(fd.sync_all())
    })();
    match at::unlinkat(&fd, staged.as_str(), AtFlags::empty()) {
        Ok(()) => io(fd.sync_all())?,
        Err(rustix::io::Errno::NOENT) => (),
        Err(e) => return Err(error(400, e.to_string())),
    }
    result
}
async fn worktree(repo: &Repo, data_dir: &Path, name: &str) -> Result<Value> {
    if !repo.prefix.is_empty() {
        return Err(error(400,"Worktree creation requires a binding to the repository root; a subdirectory binding cannot copy sibling files."));
    }
    io(workspace::open_directory(data_dir))?;
    if !data_dir.starts_with(repo.root.parent().ok_or_else(Failure::storage)?)
        || data_dir.starts_with(&repo.directory)
    {
        return Err(error(400,"Managed data storage must be outside the workspace and inside the repository root parent. Host approval is required for another location."));
    }
    let head = repo
        .head
        .as_deref()
        .ok_or_else(|| error(400, "Commit the repository before creating a worktree."))?;
    let entries = tree(repo, head).await?;
    if entries.len() > 1000 {
        return Err(error(400, "Worktree exceeds 1000 regular files."));
    }
    let mut contents = Vec::new();
    let mut total = 0;
    for entry in entries {
        let bytes = run(
            &repo.directory,
            &args(&["cat-file", "blob", &entry.hash]),
            &[],
            10 * 1024 * 1024,
            false,
        )
        .await?
        .0;
        total += bytes.len();
        if total > 32 * 1024 * 1024 {
            return Err(error(400, "Worktree exceeds 32 MiB."));
        }
        contents.push((entry, bytes));
    }
    let mut parent = data_dir.to_owned();
    let key = hash(&json!(repo.root));
    for part in ["worktrees", &key[..16]] {
        parent.push(part);
        match fs::DirBuilder::new().mode(0o700).create(&parent) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(error(400, e.to_string())),
        }
        io(workspace::open_directory(&parent))?;
    }
    let location = parent.join(uuid::Uuid::new_v4().to_string());
    command(
        &repo.directory,
        &args(&[
            "worktree",
            "add",
            "--no-checkout",
            "-b",
            name,
            "--",
            location.to_str().ok_or_else(Failure::storage)?,
            head,
        ]),
        &[],
        &[],
    )
    .await?;
    io(workspace::open_directory(&location))?;
    command(&location, &args(&["read-tree", head]), &[], &[]).await?;
    for (entry, bytes) in contents {
        publish(&location, &entry.path, &bytes, &entry.mode, false)?;
    }
    Ok(json!({"created":true,"branch":name,"path":location}))
}
pub async fn execute(
    state: &AppState,
    binding: &WorkspaceConfig,
    turn: &str,
    call: &Value,
    value: &Value,
    after_action: i64,
) -> Result<Value> {
    let _lock = WRITES.lock().await;
    let repo = repository(binding, true).await?;
    let name = text(&call["function"], "name");
    let id = text(call, "id");
    if !["git_commit", "git_create_branch", "git_create_worktree"].contains(&name) {
        return Err(error(400, "Git tool is unavailable."));
    }
    if id.is_empty() || id.encode_utf16().count() > 200 {
        return Err(error(400, "Invalid Git tool call ID."));
    }
    if let Some(old) = begin(
        state,
        turn,
        id,
        &json!([binding.agent, repo.directory, name, value]),
        Some(call),
        after_action,
    )? {
        return Ok(old);
    }
    let result = async {
        if name == "git_commit" {
            return commit(&repo, value).await;
        }
        only(value, &["name"])?;
        let branch = text(value, "name");
        if branch.is_empty()
            || branch.encode_utf16().count() > 200
            || branch.starts_with('-')
            || branch.starts_with("refs/")
            || branch.contains("@{")
            || branch.chars().any(|c| c <= ' ' || c == '\x7f')
        {
            return Err(error(400, "Invalid local branch name."));
        }
        command(
            &repo.directory,
            &args(&["check-ref-format", &format!("refs/heads/{branch}")]),
            &[],
            &[],
        )
        .await?;
        if name == "git_create_worktree" {
            return worktree(
                &repo,
                state
                    .config
                    .database
                    .parent()
                    .ok_or_else(Failure::storage)?,
                branch,
            )
            .await;
        }
        command(
            &repo.directory,
            &args(&[
                "branch",
                "--no-track",
                "--",
                branch,
                repo.head.as_deref().unwrap_or("HEAD"),
            ]),
            &[],
            &[],
        )
        .await?;
        Ok(json!({"created":true,"branch":branch}))
    }
    .await;
    finish(state, turn, id, result)
}
pub async fn restore(
    state: &AppState,
    binding: &WorkspaceConfig,
    turn: &str,
    value: &Value,
) -> Result<Value> {
    only(value, &["checkpointId", "digest"])?;
    let checkpoint = text(value, "checkpointId");
    let digest = text(value, "digest");
    if !valid_id(checkpoint)
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(error(400, "Invalid checkpoint restoration."));
    }
    let _lock = WRITES.lock().await;
    let repo = repository(binding, true).await?;
    if let Some(old) = begin(
        state,
        turn,
        "restore",
        &json!([binding.agent, repo.directory, turn, value]),
        None,
        0,
    )? {
        return Ok(old);
    }
    let result = async {
        let plan = plan(state, &repo, &binding.agent, checkpoint).await?;
        if plan.digest != digest {
            return Err(error(
                409,
                "Workspace changed after confirmation. Preview the file list again.",
            ));
        }
        capture_repo(
            state,
            &repo,
            &binding.agent,
            turn,
            &plan
                .before
                .iter()
                .map(|e| e.path.clone())
                .collect::<Vec<_>>(),
        )
        .await?;
        for file in &plan.files {
            check_path(&repo.directory, text(file, "path"))?;
        }
        let mut contents = Vec::new();
        for file in &plan.files {
            let path = text(file, "path");
            let entry = plan.before.iter().find(|e| e.path == path);
            let bytes = if let Some(entry) = entry {
                run(
                    &repo.directory,
                    &args(&["cat-file", "blob", &entry.hash]),
                    &[],
                    10 * 1024 * 1024,
                    false,
                )
                .await?
                .0
            } else {
                Vec::new()
            };
            contents.push((path, entry, bytes));
        }
        for (file, entry, bytes) in contents {
            if let Some(entry) = entry {
                publish(&repo.directory, file, &bytes, &entry.mode, true)?;
            } else {
                let (parent, name) = file.rsplit_once('/').unwrap_or(("", file));
                let directory = if parent.is_empty() {
                    repo.directory.clone()
                } else {
                    repo.directory.join(parent)
                };
                let fd = io(workspace::open_directory(&directory))?;
                io(at::unlinkat(&fd, name, AtFlags::empty()).map_err(Into::into))?;
                io(fd.sync_all())?;
            }
        }
        Ok(json!({"restored":true,"turn":checkpoint,"files":plan.files,"safetyCheckpoint":turn}))
    }
    .await;
    finish(state, turn, "restore", result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    pub(super) fn crash_ref_publication(root: &Path, phase: &str) {
        if std::env::var_os("MINARBOT_TEST_GIT_REPO").as_deref() == Some(root.as_os_str())
            && std::env::var("MINARBOT_TEST_GIT_PHASE").as_deref() == Ok(phase)
        {
            rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::KILL)
                .unwrap_or_else(|e| panic!("{e}"));
        }
    }

    fn state_at(data: &Path) -> Result<AppState> {
        let config: crate::config::Config =
            serde_json::from_value(json!({"database":data.join("app.sqlite")}))
                .map_err(|_| Failure::storage())?;
        let database =
            crate::db::open(&config.database, false, 0).map_err(|_| Failure::storage())?;
        crate::files::init(&database.connection).map_err(|_| Failure::storage())?;
        crate::data::initialize(&database).map_err(|_| Failure::storage())?;
        crate::computer::hub::initialize(&database.connection).map_err(|_| Failure::storage())?;
        let computer =
            Arc::new(crate::computer::Manager::new(&config).map_err(|_| Failure::storage())?);
        Ok(AppState {
            database: Arc::new(Mutex::new(database)),
            config: Arc::new(config),
            origin: "http://127.0.0.1".to_owned(),
            auth: None,
            events: crate::events::Hub::new(),
            computer,
            turns: crate::turns::Runtime::default(),
            whatsapp: Default::default(),
            trial_pending: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    async fn init_repository(root: &Path) -> Result<()> {
        io(fs::create_dir(root))?;
        for setup in [
            args(&["init", "-q", "--template=", "--initial-branch=main"]),
            args(&["config", "user.name", "Fixture"]),
            args(&["config", "user.email", "fixture@example.invalid"]),
        ] {
            command(root, &setup, &[], &[]).await?;
        }
        for file in ["note", "other"] {
            io(fs::write(root.join(file), "original\n"))?;
        }
        command(root, &args(&["add", "--", "note", "other"]), &[], &[]).await?;
        command(root, &args(&["commit", "-q", "-m", "Initial"]), &[], &[]).await?;
        Ok(())
    }

    #[tokio::test]
    async fn killed_commit_before_or_after_ref_publication_is_never_replayed() -> Result<()> {
        use std::os::unix::process::ExitStatusExt;
        let agent = "10000000-0000-4000-8000-000000000001";
        let turn = "10000000-0000-4000-8000-000000000002";
        let value = json!({"message":"Uncertain commit"});
        let call = json!({"id":"commit","type":"function","function":{"name":"git_commit","arguments":value.to_string()}});
        let binding = |directory| WorkspaceConfig {
            agent: agent.into(),
            directory,
            write: true,
            managed: false,
        };
        if let Some(workspace) = std::env::var_os("MINARBOT_TEST_GIT_REPO") {
            let workspace = PathBuf::from(workspace);
            let state = state_at(workspace.parent().ok_or_else(Failure::storage)?)?;
            let result = execute(&state, &binding(workspace), turn, &call, &value, 0).await;
            panic!("ref publication fault not reached: {result:?}");
        }
        for phase in ["before", "after"] {
            let dir = Temporary(
                std::env::temp_dir().join(format!("bs-git-crash-{}", uuid::Uuid::new_v4())),
            );
            io(fs::create_dir(&dir.0))?;
            let workspace = dir.0.join("repo");
            init_repository(&workspace).await?;
            let original = output(&workspace, &["rev-parse", "HEAD"], &[]).await?;
            let state = state_at(&dir.0)?;
            db(&state, |db| {
                db.execute(
                    "INSERT INTO agents(id,name,model,instructions) VALUES(?,'Fixture','','')",
                    [agent],
                )?;
                db.execute(
                    "INSERT INTO turns(id,agent,text,status,created) VALUES(?,?,'','done','')",
                    [turn, agent],
                )?;
                Ok(())
            })?;
            drop(state);
            io(fs::write(workspace.join("note"), b"uncertain\n"))?;
            let child = io(tokio::process::Command::new(io(std::env::current_exe())?)
                .args(["--exact", "files::git::writes::tests::killed_commit_before_or_after_ref_publication_is_never_replayed"])
                .env("MINARBOT_TEST_GIT_REPO", &workspace)
                .env("MINARBOT_TEST_GIT_PHASE", phase)
                .env("GIT_AUTHOR_NAME", "Injected identity")
                .output().await)?;
            assert_eq!(child.status.signal(), Some(9), "{child:?}");
            let head = output(&workspace, &["rev-parse", "HEAD"], &[]).await?;
            assert_eq!(head == original, phase == "before");
            assert_eq!(
                output(&workspace, &["log", "-1", "--format=%an"], &[]).await?,
                "Fixture"
            );
            assert_eq!(
                output(&workspace, &["log", "-1", "--format=%s"], &[]).await?,
                if phase == "before" {
                    "Initial"
                } else {
                    "Uncertain commit"
                }
            );
            let state = state_at(&dir.0)?;
            let binding = binding(workspace.clone());
            for _ in 0..2 {
                let result = execute(&state, &binding, turn, &call, &value, 0).await?;
                assert_eq!(result["uncertain"], true);
            }
            assert_eq!(
                db(&state, |db| db
                    .query_row(
                        "SELECT count(*) FROM flow_tool_calls WHERE turn=? AND call_id='commit'",
                        [turn],
                        |r| r.get::<_, i64>(0)
                    )
                    .map_err(Into::into))?,
                1
            );
            assert_eq!(output(&workspace, &["rev-parse", "HEAD"], &[]).await?, head);
            assert_eq!(io(fs::read(workspace.join("note")))?, b"uncertain\n");
        }
        Ok(())
    }

    #[tokio::test]
    async fn scoped_git_reads_bound_diff_and_fence_untrusted_paths() -> Result<()> {
        use super::super::{diff, log, status};
        let dir =
            Temporary(std::env::temp_dir().join(format!("bs-git-read-{}", uuid::Uuid::new_v4())));
        io(fs::create_dir(&dir.0))?;
        let workspace = dir.0.join("repo");
        init_repository(&workspace).await?;
        let agent = uuid::Uuid::new_v4().to_string();
        let binding = WorkspaceConfig {
            agent: agent.clone(),
            directory: workspace.clone(),
            write: true,
            managed: false,
        };
        assert_eq!(status(None).await?, json!({"isRepo":false}));
        io(fs::write(workspace.join("note"), b"staged\n"))?;
        command(&workspace, &args(&["add", "--", "note"]), &[], &[]).await?;
        io(fs::write(workspace.join("note"), b"after\n"))?;
        io(fs::write(workspace.join("new"), b"new text\n"))?;
        let snapshot = status(Some(&binding)).await?;
        for (key, expected) in [
            ("dirtyCount", 2),
            ("stagedCount", 1),
            ("unstagedCount", 1),
            ("untrackedCount", 1),
        ] {
            assert_eq!(snapshot[key], expected);
        }
        assert_eq!(snapshot["branch"], "main");
        assert_eq!(snapshot["root"], workspace.to_string_lossy().as_ref());
        assert!(snapshot["aheadCount"].is_null() && snapshot["behindCount"].is_null());
        let patch = diff(&binding, None).await?;
        let contents = text(&patch, "text");
        for line in ["-original", "+after", "+new text"] {
            assert!(contents.contains(line));
        }
        assert_eq!(patch["truncated"], false);
        io(fs::write(workspace.join("new"), b"\0BINARY"))?;
        let binary = diff(&binding, Some("new")).await?;
        assert_eq!(binary["text"], "");
        assert_eq!(binary["binaryFiles"], json!(["new"]));
        io(fs::write(
            workspace.join("large"),
            "long line ".repeat(40_000),
        ))?;
        let large = diff(&binding, Some("large")).await?;
        assert_eq!(large["truncated"], true);
        assert!(text(&large, "text").len() <= 256 * 1024 + 3);
        let commits = log(&binding, 1).await?;
        assert_eq!(commits["commits"][0]["subject"], "Initial");
        assert_eq!(text(&commits["commits"][0], "hash").len(), 40);
        assert!(log(&binding, 0).await.is_err() && log(&binding, 51).await.is_err());

        let secret = dir.0.join("private");
        io(fs::write(&secret, b"PRIVATE"))?;
        io(std::os::unix::fs::symlink(&secret, workspace.join("link")))?;
        io(std::os::unix::fs::symlink(
            &dir.0,
            workspace.join("linked-dir"),
        ))?;
        io(fs::hard_link(&secret, workspace.join("hard")))?;
        let repo = repository(&binding, true).await?;
        for path in [
            "../private",
            "/etc/passwd",
            ".git/config",
            "link",
            "hard",
            "linked-dir/private",
            "a/../private",
            "a//b",
            "bad\\path",
        ] {
            assert!(diff(&binding, Some(path)).await.is_err(), "{path}");
            assert!(
                commit(&repo, &json!({"message":"Unsafe","paths":[path]}))
                    .await
                    .is_err(),
                "{path}"
            );
        }
        assert_eq!(io(fs::read(&secret))?, b"PRIVATE");
        io(fs::write(workspace.join(":(top)private"), b"literal file"))?;
        assert_eq!(
            commit(
                &repo,
                &json!({"message":"Literal","paths":[":(top)private"]})
            )
            .await?["committed"],
            true
        );
        assert_eq!(
            output(&workspace, &["show", "HEAD::(top)private"], &[]).await?,
            "literal file"
        );
        command(
            &workspace,
            &args(&["config", "core.sparseCheckout", "true"]),
            &[],
            &[],
        )
        .await?;
        assert!(commit(
            &repository(&binding, true).await?,
            &json!({"message":"Sparse","paths":["note"]})
        )
        .await
        .is_err_and(|e| e.message.contains("Sparse checkouts")));
        command(
            &workspace,
            &args(&["config", "core.sparseCheckout", "false"]),
            &[],
            &[],
        )
        .await?;
        command(&workspace, &args(&["checkout", "-q", "--detach"]), &[], &[]).await?;
        assert!(status(Some(&binding)).await?["branch"].is_null());
        let empty = dir.0.join("empty");
        io(fs::create_dir(&empty))?;
        command(
            &empty,
            &args(&["init", "-q", "--template=", "--initial-branch=main"]),
            &[],
            &[],
        )
        .await?;
        let unborn = WorkspaceConfig {
            directory: empty.clone(),
            ..binding
        };
        assert!(status(Some(&unborn)).await?["head"].is_null());
        assert_eq!(log(&unborn, 1).await?, json!({"commits":[]}));
        let state = state_at(&dir.0)?;
        let turn = uuid::Uuid::new_v4().to_string();
        io(fs::write(empty.join("untracked"), b"unborn checkpoint"))?;
        capture(&state, &unborn, &turn).await?;
        assert_eq!(
            output(
                &empty,
                &[
                    "show",
                    &format!("refs/minarbot/checkpoints/{turn}:untracked")
                ],
                &[]
            )
            .await?,
            "unborn checkpoint"
        );
        Ok(())
    }

    #[tokio::test]
    async fn git_writes_preserve_staging_checkpoints_receipts_and_sandbox() -> Result<()> {
        let root = Temporary(
            std::env::temp_dir().join(format!("minarbot-rust-git-{}", uuid::Uuid::new_v4())),
        );
        io(fs::create_dir(&root.0))?;
        let workspace = root.0.join("repo");
        let data = root.0.join("data");
        io(fs::create_dir(&data))?;
        init_repository(&workspace).await?;
        let state = state_at(&data)?;
        let agent = uuid::Uuid::new_v4().to_string();
        let turn = uuid::Uuid::new_v4().to_string();
        db(&state, |db| {
            db.execute(
                "INSERT INTO agents(id,name,model,instructions) VALUES(?,'Fixture','','')",
                [&agent],
            )?;
            db.execute(
                "INSERT INTO turns(id,agent,text,status,created) VALUES(?,?,'','done','')",
                [&turn, &agent],
            )?;
            Ok(())
        })?;
        let binding = WorkspaceConfig {
            agent: agent.clone(),
            directory: workspace.clone(),
            write: true,
            managed: false,
        };
        let readonly = WorkspaceConfig {
            write: false,
            ..binding.clone()
        };
        assert!(capture(&state, &readonly, &turn).await.is_err());
        for name in ["git_commit", "git_create_branch", "git_create_worktree"] {
            let value = json!({"name":"denied","message":"Denied"});
            let call = json!({"id":name,"function":{"name":name,"arguments":value.to_string()}});
            assert!(execute(&state, &readonly, &turn, &call, &value, 0)
                .await
                .is_err_and(|error| error.message.contains("write permission")));
        }
        io(fs::write(workspace.join("other"), "staged\n"))?;
        command(&workspace, &args(&["add", "--", "other"]), &[], &[]).await?;
        let initial_head = output(&workspace, &["rev-parse", "HEAD"], &[]).await?;
        let initial_index = io(fs::read(workspace.join(".git/index")))?;
        let marker = dir_marker(&root.0, &workspace).await?;
        capture(&state, &binding, &turn).await?;
        assert_eq!(
            output(&workspace, &["rev-parse", "HEAD"], &[]).await?,
            initial_head
        );
        assert_eq!(io(fs::read(workspace.join(".git/index")))?, initial_index);
        io(fs::write(workspace.join("note"), "changed\n"))?;
        io(fs::write(workspace.join("new"), "untracked\n"))?;
        let value = json!({"message":"Selected","paths":["note"]});
        let call = json!({"id":"commit","type":"function","function":{"name":"git_commit","arguments":value.to_string()}});
        let committed = execute(&state, &binding, &turn, &call, &value, 0).await?;
        assert_eq!(committed["committed"], true, "{committed}");
        assert_eq!(
            output(&workspace, &["show", "HEAD:other"], &[]).await?,
            "original"
        );
        assert_eq!(
            output(&workspace, &["show", ":other"], &[]).await?,
            "staged"
        );
        assert_eq!(
            output(&workspace, &["show", "HEAD:note"], &[]).await?,
            "changed"
        );
        assert_eq!(
            execute(&state, &binding, &turn, &call, &value, 0).await?,
            committed
        );
        assert!(execute(
            &state,
            &binding,
            &turn,
            &call,
            &json!({"message":"Different"}),
            0
        )
        .await
        .is_err());
        drop(state);
        let state = state_at(&data)?;
        assert_eq!(
            execute(&state, &binding, &turn, &call, &value, 0).await?,
            committed
        );
        assert_eq!(
            output(&workspace, &["log", "-1", "--format=%an <%ae>"], &[]).await?,
            "Fixture <fixture@example.invalid>"
        );
        assert!(text(
            &crate::files::git::diff(&binding, Some("note")).await?,
            "text"
        )
        .is_empty());
        let plan_value = preview(&state, &binding, &turn).await?;
        let restore_id = uuid::Uuid::new_v4().to_string();
        let request = json!({"checkpointId":turn,"digest":plan_value["digest"]});
        let restored = restore(&state, &binding, &restore_id, &request).await?;
        assert_eq!(restored["restored"], true, "{restored}");
        assert_eq!(io(fs::read(workspace.join("note")))?, b"original\n");
        assert!(!workspace.join("new").exists());
        assert_eq!(
            restore(&state, &binding, &restore_id, &request).await?,
            restored
        );
        let undo = preview(&state, &binding, &restore_id).await?;
        assert_eq!(
            restore(
                &state,
                &binding,
                &uuid::Uuid::new_v4().to_string(),
                &json!({"checkpointId":restore_id,"digest":undo["digest"]})
            )
            .await?["restored"],
            true
        );
        assert_eq!(io(fs::read(workspace.join("note")))?, b"changed\n");
        assert_eq!(io(fs::read(workspace.join("new")))?, b"untracked\n");
        let value = json!({"name":"fixture/worktree"});
        let call = json!({"id":"worktree","function":{"name":"git_create_worktree","arguments":value.to_string()}});
        let created = execute(&state, &binding, &turn, &call, &value, 0).await?;
        assert_eq!(created["created"], true, "{created}");
        assert!(Path::new(text(&created, "path")).starts_with(data.join("worktrees")));
        assert_eq!(
            io(fs::read(Path::new(text(&created, "path")).join("note")))?,
            b"changed\n"
        );
        drop(state);
        let state = state_at(&data)?;
        assert_eq!(
            execute(&state, &binding, &turn, &call, &value, 0).await?,
            created
        );
        assert!(
            !marker.exists(),
            "a hook, filter, text converter or signing helper ran"
        );
        let value = json!({"name":"fixture/branch"});
        let call = json!({"id":"branch","function":{"name":"git_create_branch","arguments":value.to_string()}});
        assert_eq!(
            execute(&state, &binding, &turn, &call, &value, 0).await?["created"],
            true
        );
        assert_eq!(
            output(&workspace, &["branch", "--show-current"], &[]).await?,
            "main"
        );
        let status = crate::files::git::status(Some(&binding)).await?;
        assert_eq!(status["worktrees"].as_array().map(Vec::len), Some(2));
        assert!(status["localBranches"]
            .as_array()
            .is_some_and(|branches| branches.contains(&json!("fixture/branch"))));
        assert_eq!(
            execute(&state, &binding, &turn, &call, &value, 0).await?["created"],
            true
        );
        for name in ["-force", "../escape", "x..y", "a@{b", "x.lock", "/absolute"] {
            let value = json!({"name":name});
            let call = json!({"id":format!("bad-branch-{name}"),"function":{"name":"git_create_branch","arguments":value.to_string()}});
            assert_eq!(
                execute(&state, &binding, &turn, &call, &value, 0).await?["uncertain"],
                true
            );
        }
        let repo = repository(&binding, true).await?;
        assert!(worktree(&repo, &workspace, "denied")
            .await
            .is_err_and(|error| error.message.contains("Host approval")));
        io(std::os::unix::fs::symlink(
            "/etc/passwd",
            workspace.join("link"),
        ))?;
        assert!(capture(&state, &binding, &uuid::Uuid::new_v4().to_string())
            .await
            .is_err());
        let failed = preview(&state, &binding, &turn).await;
        assert!(failed.is_err());
        let prepared = uuid::Uuid::new_v4().to_string();
        begin(
            &state,
            &prepared,
            "restore",
            &json!(["interrupted"]),
            None,
            0,
        )?;
        initialize(
            &state
                .database
                .lock()
                .map_err(|_| Failure::storage())?
                .connection,
        )
        .map_err(|_| Failure::storage())?;
        assert_eq!(
            begin(
                &state,
                &prepared,
                "restore",
                &json!(["interrupted"]),
                None,
                0
            )?
            .ok_or_else(Failure::storage)?["uncertain"],
            true
        );
        Ok(())
    }

    async fn dir_marker(root: &Path, repo: &Path) -> Result<PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        let marker = root.join("git-external-command");
        let canary = root.join("marker.sh");
        io(fs::write(
            &canary,
            format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        ))?;
        io(fs::set_permissions(
            &canary,
            fs::Permissions::from_mode(0o700),
        ))?;
        let hooks = repo.join(".git/hooks");
        io(fs::create_dir_all(&hooks))?;
        for name in [
            "pre-commit",
            "prepare-commit-msg",
            "commit-msg",
            "post-commit",
            "post-checkout",
            "reference-transaction",
        ] {
            io(fs::hard_link(&canary, hooks.join(name)))?;
        }
        let config = canary.to_str().ok_or_else(Failure::storage)?;
        for (key, value) in [
            ("filter.unsafe.clean", config),
            ("filter.unsafe.smudge", config),
            ("diff.unsafe.textconv", config),
            ("commit.gpgSign", "true"),
            ("gpg.program", config),
            ("extensions.worktreeConfig", "true"),
        ] {
            command(repo, &args(&["config", key, value]), &[], &[]).await?;
        }
        for key in ["filter.worktree.clean", "filter.worktree.smudge"] {
            command(
                repo,
                &args(&["config", "--worktree", key, config]),
                &[],
                &[],
            )
            .await?;
        }
        io(fs::write(
            repo.join(".gitattributes"),
            "note filter=unsafe diff=unsafe\n",
        ))?;
        io(fs::create_dir_all(repo.join(".git/info")))?;
        io(fs::write(
            repo.join(".git/info/attributes"),
            "note filter=worktree\n",
        ))?;
        Ok(marker)
    }

    #[tokio::test]
    async fn git_scoped_commit_restore_and_pruning_preserve_siblings() -> Result<()> {
        use super::super::{diff, status};
        let dir =
            Temporary(std::env::temp_dir().join(format!("bs-git-scope-{}", uuid::Uuid::new_v4())));
        io(fs::create_dir(&dir.0))?;
        let workspace = dir.0.join("repo");
        init_repository(&workspace).await?;
        let scoped = workspace.join("scoped");
        io(fs::create_dir(&scoped))?;
        io(fs::write(scoped.join("in"), b"inside before\n"))?;
        io(fs::write(scoped.join(".gitignore"), b"ignored\n"))?;
        command(&workspace, &args(&["add", "--", "scoped"]), &[], &[]).await?;
        command(&workspace, &args(&["commit", "-qm", "Scope"]), &[], &[]).await?;
        let agent = uuid::Uuid::new_v4().to_string();
        let binding = WorkspaceConfig {
            agent: agent.clone(),
            directory: scoped.clone(),
            write: true,
            managed: false,
        };
        let state = state_at(&dir.0)?;
        io(fs::write(scoped.join("ignored"), b"keep ignored"))?;
        io(fs::write(workspace.join("other"), b"OUTSIDE SECRET"))?;
        io(fs::write(scoped.join("in"), b"inside after\n"))?;
        assert_eq!(
            status(Some(&binding)).await?["changedFiles"],
            json!([{"code":" M","path":"in"}])
        );
        assert!(!diff(&binding, None)
            .await?
            .to_string()
            .contains("OUTSIDE SECRET"));
        let original = output(&workspace, &["rev-parse", "HEAD"], &[]).await?;
        let index = io(fs::read(workspace.join(".git/index")))?;
        let old = (0..50)
            .map(|_| uuid::Uuid::new_v4().to_string())
            .collect::<Vec<_>>();
        let mut refs = String::new();
        for (index, id) in old.iter().enumerate() {
            let reference = format!("refs/minarbot/checkpoints/{id}");
            refs.push_str(&format!("create {reference} {original}\n"));
            db(&state, |db| {
                db.execute(
                    "INSERT INTO workspace_checkpoints VALUES(?,?,?,?,?,'done',?)",
                    rusqlite::params![
                        id,
                        agent,
                        scoped.to_str(),
                        reference,
                        original,
                        index as i64 - 50
                    ],
                )?;
                Ok(())
            })?;
        }
        command(
            &workspace,
            &args(&["update-ref", "--stdin"]),
            &[],
            refs.as_bytes(),
        )
        .await?;
        drop(state);
        let state = state_at(&dir.0)?;
        let turn = uuid::Uuid::new_v4().to_string();
        io(fs::write(
            scoped.join("untracked"),
            b"untracked in checkpoint",
        ))?;
        capture(&state, &binding, &turn).await?;
        assert_eq!(
            output(&workspace, &["rev-parse", "HEAD"], &[]).await?,
            original
        );
        assert_eq!(io(fs::read(workspace.join(".git/index")))?, index);
        assert_eq!(
            output(
                &workspace,
                &[
                    "for-each-ref",
                    "--format=%(refname)",
                    "refs/minarbot/checkpoints"
                ],
                &[]
            )
            .await?
            .lines()
            .count(),
            50
        );
        assert!(preview(&state, &binding, &old[0]).await.is_err());
        assert_eq!(
            output(
                &workspace,
                &[
                    "show",
                    &format!("refs/minarbot/checkpoints/{turn}:untracked")
                ],
                &[]
            )
            .await?,
            "untracked in checkpoint"
        );
        io(fs::write(scoped.join("in"), b"current"))?;
        let stale = preview(&state, &binding, &turn).await?;
        io(fs::write(scoped.join("in"), b"raced"))?;
        let rejected = restore(
            &state,
            &binding,
            &uuid::Uuid::new_v4().to_string(),
            &json!({"checkpointId":turn,"digest":stale["digest"]}),
        )
        .await;
        assert!(
            rejected
                .as_ref()
                .err()
                .is_some_and(|e| e.message.contains("changed"))
                || rejected
                    .as_ref()
                    .ok()
                    .is_some_and(|v| text(v, "error").contains("changed"))
        );
        assert_eq!(io(fs::read(scoped.join("in")))?, b"raced");
        let latest = preview(&state, &binding, &turn).await?;
        let restore_id = uuid::Uuid::new_v4().to_string();
        let request = json!({"checkpointId":turn,"digest":latest["digest"]});
        let restored = restore(&state, &binding, &restore_id, &request).await?;
        assert_eq!(restored["restored"], true);
        assert_eq!(io(fs::read(scoped.join("in")))?, b"inside after\n");
        assert_eq!(io(fs::read(scoped.join("ignored")))?, b"keep ignored");
        assert_eq!(io(fs::read(workspace.join("other")))?, b"OUTSIDE SECRET");
        assert_eq!(io(fs::read(workspace.join(".git/index")))?, index);
        let repo = repository(&binding, true).await?;
        assert_eq!(
            commit(&repo, &json!({"message":"Scope commit"})).await?["committed"],
            true
        );
        assert_eq!(
            output(&workspace, &["show", "HEAD:other"], &[]).await?,
            "original"
        );
        assert_eq!(
            output(&workspace, &["show", "HEAD:scoped/in"], &[]).await?,
            "inside after"
        );
        assert!(
            output(&workspace, &["ls-tree", "-r", "--name-only", "HEAD"], &[])
                .await?
                .lines()
                .all(|name| name != "scoped/untracked")
        );
        assert!(commit(
            &repository(&binding, true).await?,
            &json!({"message":"No changes"})
        )
        .await
        .is_err_and(|e| e.message.contains("Nothing to commit")));
        assert!(worktree(&repo, &dir.0, "forbidden")
            .await
            .is_err_and(|e| e.message.contains("binding to the repository root")));
        Ok(())
    }
}
