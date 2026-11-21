use super::*;
use std::sync::LazyLock;
use std::{os::fd::AsRawFd, process::Stdio};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

static COMMAND_REQUIRING_APPROVAL: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)(?:\b(?:rm|rmdir|shred|dd|mkfs|sudo|su|mount|umount|chmod|chown|kill|pkill|killall|curl|wget|ssh|scp|sftp|sendmail|mail)\b|\bgit\s+(?:push|reset|clean|restore|checkout)\b|\bnpm\s+(?:publish|unpublish|login|token)\b|\b(?:printenv|env)\b|\.(?:ssh|aws|codex)|credentials|secrets?\b)").unwrap_or_else(|_| unreachable!())
});

pub fn decide(db: &Connection, turn: &str, approval: &str, v: &Value) -> Result<Value> {
    if !closed(v, &["id", "revision", "decision"])
        || !integer(&v["revision"], 1, 9_007_199_254_740_991)
        || !["allow", "always", "deny"].contains(&string(v, "decision"))
    {
        return Err(error(400, "Invalid harness approval decision."));
    }
    let request =
        json!({"turn":turn,"approval":approval,"revision":v["revision"],"decision":v["decision"]});
    if let Some(prior) = data::receipt(
        db,
        "harness_decisions",
        string(v, "id"),
        &request,
        "Request ID already used for another approval.",
    )? {
        return Ok(prior);
    }
    let row = one(
        db,
        "SELECT * FROM harness_approvals WHERE id=? AND turn=?",
        [approval, turn],
    )?
    .filter(|r| r["state"] == "approval" && r["revision"] == v["revision"])
    .ok_or_else(|| {
        error(
            409,
            "Approval expired or task stopped. Ask again in a new turn.",
        )
    })?;
    if !exists(
        db,
        "SELECT 1 FROM turns WHERE id=? AND status='running'",
        [turn],
    )? {
        return Err(error(
            409,
            "Approval expired or task stopped. Ask again in a new turn.",
        ));
    }
    if v["decision"] == "always" && row["prefix"].is_null() {
        return Err(error(
            400,
            "Only literal command prefixes support Always allow.",
        ));
    }
    if row["kind"] == "email" {
        crate::connectors::mailbox::decide_draft(db, &row, v["decision"] == "allow")?;
    }
    db.execute(
        "UPDATE harness_approvals SET state='decided',decision=?,revision=revision+1 WHERE id=?",
        [string(v, "decision"), approval],
    )?;
    if v["decision"] == "always" {
        db.execute(
            "INSERT OR IGNORE INTO harness_policies VALUES (?,?,?,?)",
            params![
                row["agent"].as_str(),
                row["runtime"].as_str(),
                row["workspace"].as_str(),
                row["prefix"].as_str()
            ],
        )?;
    }
    let result = json!({"state":"decided","decision":v["decision"],"revision":row["revision"].as_i64().unwrap_or(1)+1});
    data::save_receipt(db, "harness_decisions", string(v, "id"), &request, &result)?;
    Ok(result)
}
async fn approval(
    state: &AppState,
    turn: &Value,
    binding: &crate::config::WorkspaceConfig,
    command: &str,
    args: &Value,
) -> Result<bool> {
    let native = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        data::agents::read(&guard.connection, string(turn, "agent"))?["runtime"] != "api"
            && !harness::peer(&guard.connection, string(turn, "id"))?
    };
    if native {
        if binding.write && !COMMAND_REQUIRING_APPROVAL.is_match(command) {
            return Ok(true);
        }
        return crate::harness::approve(
            state,
            string(turn, "id"),
            "command",
            Some(command),
            "Run workspace shell command?",
            args,
            "",
        )
        .await;
    }
    let id = uuid::Uuid::new_v4().to_string();
    let mut changes = state.events.subscribe();
    let _wait = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let wait = crate::harness::config::CardWait::start(state, string(turn, "id"))?;
        guard.connection.execute("INSERT INTO harness_approvals(id,turn,agent,runtime,workspace,kind,description,command,prefix,input,state) VALUES (?,?,?,'api',?,'command','Run workspace shell command?',?,NULL,?,'approval')",params![id,turn["id"].as_str(),turn["agent"].as_str(),binding.directory.to_str(),command,args.to_string()])?;
        wait
    };
    loop {
        {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            let row = one(
                &guard.connection,
                "SELECT state,decision FROM harness_approvals WHERE id=?",
                [&id],
            )?
            .ok_or_else(Failure::storage)?;
            if row["state"] == "decided" {
                return Ok(row["decision"] != "deny");
            }
            if row["state"] != "approval" {
                return Err(error(
                    409,
                    "Harness approval expired because this turn stopped.",
                ));
            }
        }
        tokio::select! {_=changes.recv()=>(),_=tokio::time::sleep(std::time::Duration::from_millis(200))=>()}
    }
}
pub async fn run(state: &AppState, turn: &Value, call: &Value, args: Value) -> Result<Value> {
    let binding = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        tools::workspace(state, &guard.connection, turn)?
            .ok_or_else(|| error(403, "Shell access is not granted for this conversation."))?
    };
    let command = string(&args, "command");
    let cwd = args.get("cwd").and_then(Value::as_str).unwrap_or("");
    let timeout = args.get("timeoutMs").cloned().unwrap_or(json!(15000));
    if !closed(&args, &["command", "check", "cwd", "timeoutMs"])
        || command.trim().is_empty()
        || command.contains('\0')
        || command.encode_utf16().count() > 16000
        || args.get("check").is_some_and(|v| !v.is_boolean())
        || args.get("cwd").is_some_and(|v| !v.is_string())
        || cwd.encode_utf16().count() > 2048
        || cwd.starts_with('/')
        || cwd.chars().any(|c| c.is_control() || c == '\\')
        || cwd.split('/').any(|p| p == "..")
        || !integer(&timeout, 100, 60000)
    {
        return Err(error(400, "Invalid shell command or workspace cwd."));
    }
    let path = binding.directory.join(cwd);
    let actual = path
        .canonicalize()
        .map_err(|_| error(403, "Shell cwd must stay inside the workspace."))?;
    let root = binding
        .directory
        .canonicalize()
        .map_err(|_| error(403, "Shell cwd must stay inside the workspace."))?;
    if !actual.starts_with(&root) {
        return Err(error(403, "Shell cwd must stay inside the workspace."));
    }
    let pinned = crate::files::workspace::open_directory(&binding.directory)
        .map_err(|_| error(403, "Shell cwd must stay inside the workspace."))?;
    let _cwd = crate::files::workspace::open_directory(&actual)
        .map_err(|_| error(403, "Shell cwd must stay inside the workspace."))?;
    let uncertain = json!({"uncertain":true,"error":"Shell result unconfirmed. Inspect workspace effects before continuing; never replay automatically."});
    {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        tools::journal(&guard.connection, turn, call, &uncertain)?;
    }
    let result = if !approval(state, turn, &binding, command, &args).await? {
        json!({"error":"Shell command approval denied.","uncertain":true})
    } else {
        if pending(state) {
            return Err(error(503, "Update trial is pending."));
        }
        let (reader, writer) =
            std::os::unix::net::UnixStream::pair().map_err(|_| Failure::storage())?;
        reader
            .set_nonblocking(true)
            .map_err(|_| Failure::storage())?;
        let mut output =
            tokio::net::UnixStream::from_std(reader).map_err(|_| Failure::storage())?;
        let stdout: std::os::fd::OwnedFd =
            writer.try_clone().map_err(|_| Failure::storage())?.into();
        let stderr: std::os::fd::OwnedFd = writer.into();
        let mut child = Command::new("/usr/bin/bwrap")
            .kill_on_drop(true)
            .env_clear()
            .args([
                "--die-with-parent",
                "--new-session",
                "--unshare-all",
                "--ro-bind",
                "/usr",
                "/usr",
                "--symlink",
                "usr/bin",
                "/bin",
                "--symlink",
                "usr/lib",
                "/lib",
                "--symlink",
                "usr/lib64",
                "/lib64",
                "--proc",
                "/proc",
                "--dev",
                "/dev",
                "--tmpfs",
                "/tmp",
                "--dir",
                "/home",
                if binding.write { "--bind" } else { "--ro-bind" },
            ])
            .current_dir(format!("/proc/self/fd/{}", pinned.as_raw_fd()))
            .arg("/proc/self/cwd")
            .arg("/workspace")
            .args([
                "--chdir",
                &format!(
                    "/workspace{}",
                    if cwd.is_empty() {
                        String::new()
                    } else {
                        format!("/{cwd}")
                    }
                ),
                "--clearenv",
                "--setenv",
                "PATH",
                "/usr/bin:/bin",
                "--setenv",
                "HOME",
                "/tmp",
                "--setenv",
                "TMPDIR",
                "/tmp",
                "/bin/sh",
                "-s",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .map_err(|_| error(503, "Shell sandbox is unavailable."))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(format!("{}\n", command).as_bytes())
                .await
                .map_err(|_| {
                    error(
                        503,
                        "Shell execution failed. Inspect workspace effects before retrying.",
                    )
                })?;
        }
        let read = async {
            let mut bytes = Vec::new();
            let mut buf = [0; 8192];
            loop {
                let n = output
                    .read(&mut buf)
                    .await
                    .map_err(|_| Failure::storage())?;
                if n == 0 {
                    return Ok::<_, Failure>((bytes, false));
                }
                if bytes.len() + n > 256 * 1024 {
                    let remaining = (256 * 1024usize).saturating_sub(bytes.len() + 3);
                    bytes.extend_from_slice(&buf[..remaining.min(n)]);
                    return Ok((bytes, true));
                }
                bytes.extend_from_slice(&buf[..n]);
            }
        };
        let (bytes, truncated, timed_out) = match tokio::time::timeout(
            std::time::Duration::from_millis(timeout.as_u64().unwrap_or(15000)),
            read,
        )
        .await
        {
            Ok(result) => {
                let (bytes, truncated) = result?;
                if truncated {
                    let _ = child.kill().await;
                }
                (bytes, truncated, false)
            }
            Err(_) => {
                let _ = child.kill().await;
                (Vec::new(), false, true)
            }
        };
        let exit = child.wait().await.map_err(|_| Failure::storage())?;
        let mut result = json!({"exitCode":exit.code(),"output":String::from_utf8_lossy(&bytes),"timedOut":timed_out,"truncated":truncated});
        if args["check"] == true {
            result["check"] = json!(true);
        }
        result
    };
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    guard.connection.execute(
        "UPDATE flow_tool_calls SET result=? WHERE turn=? AND call_id=?",
        params![result.to_string(), turn["id"].as_str(), call["id"].as_str()],
    )?;
    super::verification::capture(
        &guard.connection,
        turn["id"].as_str().ok_or_else(Failure::storage)?,
    )?;
    let mut result = result;
    if let Some(receipt) = super::verification::receipt(
        &guard.connection,
        turn["id"].as_str().ok_or_else(Failure::storage)?,
        call["id"].as_str().ok_or_else(Failure::storage)?,
    )? {
        result["verification"] = receipt;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_and_edits_stay_automatic_while_sensitive_commands_require_approval() {
        for command in [
            "node --test",
            "npm test",
            "npm run build",
            "cat > hello.txt << EOF\nhello\nEOF",
            "git diff",
            "python3 clean_csv.py",
        ] {
            assert!(!COMMAND_REQUIRING_APPROVAL.is_match(command), "{command}");
        }
        for command in [
            "rm -rf .",
            "git push",
            "git reset --hard",
            "npm publish",
            "curl https://example.test",
            "cat ~/.ssh/id_rsa",
            "printenv",
            "sendmail someone@example.test",
        ] {
            assert!(COMMAND_REQUIRING_APPROVAL.is_match(command), "{command}");
        }
    }
}
