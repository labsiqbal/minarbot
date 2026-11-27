//! Sign-in for Codex and Claude provider instances from a browser that may
//! be on another machine. The CLI runs on this host; its sign-in link (and
//! Codex's one-time device code) are read from its output and returned, and
//! Claude's pasted authorization code is written to its stdin.

use super::{error, Result};
use regex::Regex;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    process::Stdio,
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, Command},
    sync::{watch, Mutex},
    task::JoinHandle,
};

/// How long a started sign-in may wait for the user.
const LIFETIME: Duration = Duration::from_secs(15 * 60);
/// How long the CLI gets to print its link.
const LINK_WAIT: Duration = Duration::from_secs(20);
/// How long the CLI gets to exchange a pasted code.
const CODE_WAIT: Duration = Duration::from_secs(30);

static ANSI: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").ok());
static URL: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"https://\S+").ok());
static DEVICE_CODE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\b[A-Z0-9]{4,}-[A-Z0-9]{4,}\b").ok());

#[derive(Clone, Default)]
struct Progress {
    lines: Vec<String>,
    exit: Option<i32>,
}

struct Session {
    stdin: Option<ChildStdin>,
    progress: watch::Receiver<Progress>,
    task: JoinHandle<()>,
}

static SESSIONS: LazyLock<Mutex<HashMap<String, Session>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn clean(line: &str) -> String {
    ANSI.as_ref()
        .map_or_else(
            || line.to_owned(),
            |ansi| ansi.replace_all(line, "").into_owned(),
        )
        .trim()
        .to_owned()
}

/// The sign-in link and, for Codex, the one-time code, once printed.
fn link(kind: &str, lines: &[String]) -> Option<(String, Option<String>)> {
    let url = URL.as_ref()?;
    let position = lines.iter().position(|line| url.is_match(line))?;
    let found = url.find(&lines[position])?.as_str().to_owned();
    if kind != "codex" {
        return Some((found, None));
    }
    let code = DEVICE_CODE.as_ref().and_then(|pattern| {
        lines[position + 1..]
            .iter()
            .find_map(|line| pattern.find(line).map(|code| code.as_str().to_owned()))
    })?;
    Some((found, Some(code)))
}

/// The CLI's last message, for a failed sign-in.
fn failure(lines: &[String]) -> String {
    lines
        .iter()
        .rev()
        // Claude's prompt has no newline, so its answer shares the line.
        .map(|line| {
            line.trim_start_matches("Paste code here if prompted >")
                .trim()
        })
        .find(|line| !line.is_empty())
        .map(|line| {
            line.trim_start_matches("Couldn't log in: ")
                .chars()
                .take(300)
                .collect()
        })
        .unwrap_or_else(|| "Sign-in did not finish. Start it again.".into())
}

/// Starts `command` (the CLI's login command) for instance `id`, replacing
/// an earlier sign-in for it, and returns the link to open.
pub(crate) async fn start(id: &str, kind: &str, mut command: Command) -> Result<Value> {
    let mut sessions = SESSIONS.lock().await;
    if let Some(previous) = sessions.remove(id) {
        previous.task.abort();
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| error(502, "Provider sign-in could not start."))?;
    let stdin = child.stdin.take();
    let (sender, mut receiver) = watch::channel(Progress::default());
    let sender = Arc::new(sender);
    let mut readers = Vec::new();
    for stream in [
        child
            .stdout
            .take()
            .map(|out| Box::new(out) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
        child
            .stderr
            .take()
            .map(|err| Box::new(err) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
    ]
    .into_iter()
    .flatten()
    {
        let sender = sender.clone();
        readers.push(tokio::spawn(async move {
            let mut lines = BufReader::new(stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                sender.send_modify(|progress| {
                    if progress.lines.len() < 200 {
                        progress.lines.push(clean(&line));
                    }
                });
            }
        }));
    }
    let task = tokio::spawn(async move {
        let status = tokio::time::timeout(LIFETIME, child.wait()).await;
        for reader in readers {
            let _ = reader.await;
        }
        let code = match status {
            Ok(Ok(status)) => status.code().unwrap_or(-1),
            _ => -1,
        };
        sender.send_modify(|progress| progress.exit = Some(code));
        drop(child);
    });
    let result = tokio::time::timeout(LINK_WAIT, async {
        loop {
            let found = {
                let progress = receiver.borrow_and_update();
                (
                    link(kind, &progress.lines),
                    progress.exit,
                    progress.lines.clone(),
                )
            };
            match found {
                (Some(found), _, _) => return Ok(found),
                (None, Some(_), lines) => return Err(failure(&lines)),
                _ => {}
            }
            if receiver.changed().await.is_err() {
                return Err("Sign-in did not start.".to_owned());
            }
        }
    })
    .await;
    let (url, code) = match result {
        Ok(Ok(found)) => found,
        Ok(Err(message)) => {
            task.abort();
            return Err(error(502, message));
        }
        Err(_) => {
            task.abort();
            return Err(error(504, "The provider did not show a sign-in link."));
        }
    };
    sessions.insert(
        id.to_owned(),
        Session {
            stdin,
            progress: receiver,
            task,
        },
    );
    Ok(json!({"started":true,"url":url,"code":code,"needsCode":kind == "claude"}))
}

/// Sends a pasted authorization code to the running sign-in for `id` and
/// waits for the CLI to finish.
pub(crate) async fn submit(id: &str, code: &str) -> Result<()> {
    let code = code.trim();
    if code.is_empty() || code.len() > 4096 || code.chars().any(char::is_control) {
        return Err(error(400, "Paste the code from the sign-in page."));
    }
    let mut receiver = {
        let mut sessions = SESSIONS.lock().await;
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| error(409, "This sign-in expired. Start it again."))?;
        let stdin = session
            .stdin
            .as_mut()
            .ok_or_else(|| error(409, "This sign-in expired. Start it again."))?;
        let written = async {
            stdin.write_all(code.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await
        };
        if written.await.is_err() {
            sessions.remove(id);
            return Err(error(409, "This sign-in expired. Start it again."));
        }
        session.stdin = None;
        session.progress.clone()
    };
    let finished = tokio::time::timeout(CODE_WAIT, async {
        loop {
            let progress = receiver.borrow_and_update().clone();
            if let Some(exit) = progress.exit {
                return (exit, progress.lines);
            }
            if receiver.changed().await.is_err() {
                return (-1, progress.lines);
            }
        }
    })
    .await;
    SESSIONS.lock().await.remove(id);
    match finished {
        Ok((0, _)) => Ok(()),
        Ok((_, lines)) => Err(error(400, failure(&lines))),
        Err(_) => Err(error(504, "Sign-in did not finish. Start it again.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(clean).collect()
    }

    #[test]
    fn codex_link_has_the_device_code() -> std::result::Result<(), String> {
        let output = lines(
            "Welcome to Codex [v\x1b[90m0.170.0\x1b[0m]\n\
             1. Open this link in your browser and sign in to your account\n   \x1b[94mhttps://auth.openai.com/codex/device\x1b[0m\n\n\
             2. Enter this one-time code \x1b[90m(expires in 15 minutes)\x1b[0m\n   \x1b[94m5J47-9AEJ8\x1b[0m\n",
        );
        let found = link("codex", &output).ok_or("no link")?;
        if found
            != (
                "https://auth.openai.com/codex/device".into(),
                Some("5J47-9AEJ8".into()),
            )
        {
            return Err(format!("{found:?}"));
        }
        if link("codex", &output[..3]).is_some() {
            return Err("link without its code".into());
        }
        Ok(())
    }

    #[test]
    fn claude_link_and_failure() -> std::result::Result<(), String> {
        let output = lines(
            "Opening browser to sign in…\nIf the browser didn't open, visit:\nhttps://claude.ai/oauth/authorize?code=true&state=abc\nPaste code here if prompted > ",
        );
        let found = link("claude", &output).ok_or("no link")?;
        if found
            != (
                "https://claude.ai/oauth/authorize?code=true&state=abc".into(),
                None,
            )
        {
            return Err(format!("{found:?}"));
        }
        let failed =
            lines("Paste code here if prompted > Couldn't log in: That code wasn't accepted.\n");
        if failure(&failed) != "That code wasn't accepted." {
            return Err(failure(&failed));
        }
        Ok(())
    }
}
