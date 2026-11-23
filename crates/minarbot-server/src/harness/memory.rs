use super::{
    model_catalog,
    process::{ProbeHome, Process},
    store, Result,
};
use crate::{
    data::{self, error, logins, string},
    mcp::secrets,
    web::Failure,
    AppState,
};
use rusqlite::params;
use serde_json::Value;
use std::time::Duration;

pub async fn call(state: &AppState, turn: &str, messages: &[Value], phase: &str) -> Result<String> {
    let (agent, credential, mut redactions) = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        let agent = crate::turns::memory::completion_agent(db, turn, phase)?;
        let runtime = string(&agent, "runtime");
        if !logins::device_runtime(runtime) {
            return Err(error(503, "Engine memory login is unavailable."));
        }
        let binding = string(&agent, "provider_account");
        let credential = if binding == logins::DEVICE {
            None
        } else {
            let selected = logins::bound(db, binding)?;
            if let Some(reason) = logins::blocked_reason(runtime, selected.as_ref()) {
                return Err(error(503, reason));
            }
            let selected = selected.ok_or_else(Failure::storage)?;
            Some(logins::credentials(state, &selected)?.ok_or_else(Failure::storage)?)
        };
        let redactions = crate::computer::vault::redactions(state, db)?;
        (agent, credential, redactions)
    };
    let runtime = string(&agent, "runtime");
    let binding = string(&agent, "provider_account");
    let configured = string(&agent, "memory_model");
    let main = string(&agent, "model");
    let default = if configured.is_empty() {
        if runtime == "claude" {
            model_catalog::native_memory_default(runtime, &[]).map(str::to_owned)
        } else {
            model_catalog::models(state, runtime, binding, false)
                .await
                .ok()
                .and_then(|catalog| model_catalog::memory_default(&catalog, main))
        }
    } else {
        None
    };
    let model = default.as_deref().unwrap_or(if configured.is_empty() {
        main
    } else {
        configured
    });
    let scratch = ProbeHome::new().map_err(|_| Failure::storage())?;
    let executable = super::executable(state, runtime)
        .ok_or_else(|| error(503, "Memory engine is unavailable."))?;
    let home = credential
        .as_ref()
        .map(|bytes| {
            redactions.extend(logins::secrets(runtime, bytes));
            store::home(
                &scratch.0,
                runtime,
                None,
                Some((binding, bytes)),
                None,
                "",
                String::new(),
            )
        })
        .transpose()?;
    let mut env = if let Some(home) = &home {
        home.environment.clone()
    } else {
        let (_, mut env) = logins::device_process_environment(runtime, super::path(state))
            .ok_or_else(Failure::storage)?;
        if runtime == "codex" {
            env.insert("HOME".into(), scratch.0.to_string_lossy().into_owned());
        }
        env
    };
    env.insert("PATH".into(), super::path(state));
    env.insert("TMPDIR".into(), scratch.0.to_string_lossy().into_owned());
    super::mise_environment(&executable, &mut env);
    let flat_prompt = messages
        .iter()
        .map(|m| string(m, "content"))
        .collect::<Vec<_>>()
        .join("\n\n");
    let input = secrets::redact(&flat_prompt, &redactions, false);
    let mut args = if runtime == "codex" {
        vec![
            "exec",
            "--json",
            "--ephemeral",
            "--skip-git-repo-check",
            "--ignore-user-config",
            "--ignore-rules",
            "--sandbox",
            "read-only",
            "--disable",
            "shell_tool",
            "--disable",
            "unified_exec",
            "--disable",
            "hooks",
            "--disable",
            "plugins",
            "--disable",
            "image_generation",
            "-c",
            "web_search=\"disabled\"",
            "-c",
            "approval_policy=\"never\"",
            "-c",
            "cli_auth_credentials_store=\"file\"",
        ]
    } else {
        vec![
            "--print",
            "--output-format",
            "stream-json",
            "--verbose",
            "--tools",
            "",
            "--setting-sources",
            "",
            "--strict-mcp-config",
            "--no-session-persistence",
            "--disable-slash-commands",
            "--settings",
            "{\"disableAllHooks\":true}",
        ]
    }
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    if !model.is_empty() && model != "default" {
        args.extend(["--model".into(), model.into()]);
    }
    if runtime == "codex" {
        args.push("-".into());
    }

    let call = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        if crate::turns::pending(state)
            || crate::turns::memory::completion_agent(db, turn, phase)?["revision"]
                != agent["revision"]
        {
            return Err(error(409, "Conversation learning settings changed."));
        }
        db.execute(
            "INSERT INTO provider_calls(turn,agent,model,origin,purpose,phase,family,account,status,created)
             VALUES(?,?,?,'local-harness','completion',?,?,?,'pending',?)",
            params![turn,string(&agent,"id"),model,phase,runtime,binding,data::iso(db,state.config.now())?],
        )?;
        db.last_insert_rowid()
    };
    let mut tokens = [None; 3];
    let timeout = state.config.data.timeout.unwrap_or(90_000).min(90_000);
    let result = tokio::time::timeout(Duration::from_millis(timeout), async {
        let mut child = Process::launch(&executable, &args, &scratch.0, &env)?;
        child.write_input(input.as_bytes()).await?;
        child.close_stdin();
        let mut answer = String::new();
        loop {
            let event = child.next().await?;
            if runtime == "codex" {
                match event["type"].as_str() {
                    Some("item.completed") if event["item"]["type"] == "agent_message" => {
                        answer = string(&event["item"], "text").to_owned();
                    }
                    Some("turn.completed") => {
                        tokens[0] = count(&event["usage"]["input_tokens"]);
                        tokens[1] = count(&event["usage"]["output_tokens"]);
                        break;
                    }
                    Some("turn.failed" | "error") => {
                        return Err(error(502, "Memory engine completion failed."));
                    }
                    _ => (),
                }
            } else if event["type"] == "result" {
                let usage = &event["usage"];
                tokens[0] = count(&usage["input_tokens"]).and_then(|n| {
                    n.checked_add(count(&usage["cache_read_input_tokens"]).unwrap_or(0))?
                        .checked_add(count(&usage["cache_creation_input_tokens"]).unwrap_or(0))
                });
                tokens[1] = count(&usage["output_tokens"]);
                if event["is_error"] == true {
                    return Err(error(502, "Memory engine completion failed."));
                }
                answer = string(&event, "result").to_owned();
                break;
            }
        }
        if answer.trim().is_empty() {
            return Err(error(502, "Memory engine returned no text."));
        }
        Ok(secrets::redact(&answer, &redactions, false))
    })
    .await
    .unwrap_or_else(|_| Err(error(502, "Memory engine timed out.")));
    tokens[2] = tokens[0].zip(tokens[1]).and_then(|(a, b)| a.checked_add(b));
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    db.execute(
        "UPDATE provider_calls SET status=?,finished=?,input_tokens=?,output_tokens=?,total_tokens=?
         WHERE seq=? AND status='pending'",
        params![if result.is_ok(){"received"}else{"failed"},data::iso(db,state.config.now())?,tokens[0],tokens[1],tokens[2],call],
    )?;
    result
}

fn count(value: &Value) -> Option<i64> {
    value.as_i64().filter(|v| *v >= 0)
}
