//! Local projects: folders, members, sections and migrations (port of
//! `app/local-projects.test.ts`).

use minarbot_testkit::{
    backup,
    db::{Fixture, Inspector},
    fakes::{self, Fake},
    http::Method,
    issuer::SessionIssuer,
    model::{Reply, ScriptedModel},
    prelude::*,
    providers::create_instance,
};
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

/// The Node provider: the latest user message picks one workspace tool;
/// the reply after a tool result is that result as a JSON string.
fn project_model(model: &ScriptedModel) {
    model.respond(|request| {
        let latest = request.last_user_text();
        let name = match latest.as_str() {
            "READ" => "read_workspace_file",
            "WRITE" => "create_workspace_file",
            "GIT" => "git_status",
            "EXPLORE" => "codegraph_explore",
            _ => "",
        };
        let last = request.messages().last().cloned().unwrap_or(Value::Null);
        if last["role"] == "tool" {
            return Reply::text(
                Value::String(last["content"].as_str().unwrap_or("").to_owned()).to_string(),
            );
        }
        if name.is_empty() {
            return Reply::text("No tools");
        }
        let args = match name {
            "read_workspace_file" => json!({"path": "read.txt"}),
            "create_workspace_file" => json!({"path": "made.txt", "text": "made"}),
            "codegraph_explore" => json!({"query": "projectFixture"}),
            _ => json!({}),
        };
        Reply::tool(name, args)
    });
}

struct Projects {
    app: Server,
    model: ScriptedModel,
    config: ServerConfig,
    /// The scratch directory, also the server's `HOME` so the Owner-home
    /// folder rules apply inside it, as `node_modules/.projects-test-*` did.
    directory: PathBuf,
    workspace: PathBuf,
    agent: String,
    project: Value,
}

impl Projects {
    async fn start() -> Result<Self> {
        let model = ScriptedModel::start().await?;
        project_model(&model);
        let root = TestRoot::new()?;
        let directory = root.path().to_owned();
        let workspace = root.dir("workspace")?;
        std::fs::write(workspace.join("read.txt"), "project bytes")?;
        let config = ServerConfig::new()
            .model(&model)
            .option("projectRoot", directory.join("Projects").to_string_lossy())
            .env("HOME", directory.to_string_lossy());
        let app = root.start(config.clone()).await?;
        let agent = new_id();
        app.post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": agent, "name": "Coder", "model": "fixture",
                "instructions": "Help"}),
            201,
        )
        .await?;
        let project = app
            .post(
                "/api/projects",
                json!({"id": new_id(), "name": "Project",
                    "folders": [{"mode": "existing", "path": workspace}],
                    "members": [{"agent": agent, "write": false}]}),
            )
            .await?;
        Ok(Self {
            app,
            model,
            config,
            directory,
            workspace,
            agent,
            project,
        })
    }

    fn data(&self) -> Result<PathBuf> {
        Ok(self.app.database().parent().ok_or("data dir")?.to_owned())
    }

    async fn request(&self, path: &str, body: Option<Value>, status: u16) -> Result<Value> {
        let method = if body.is_some() {
            Method::POST
        } else {
            Method::GET
        };
        self.app.call(method, path, body).await?.expect(status)
    }

    async fn send(&self, path: &str, text: &str) -> Result<Value> {
        let id = new_id();
        self.app
            .post_expect(path, json!({"id": id, "text": text}), 202)
            .await?;
        self.app
            .wait_row(path, &id, |row| {
                ["done", "failed"].contains(&row["status"].as_str().unwrap_or(""))
            })
            .await
    }

    async fn restart(&mut self, config: ServerConfig) -> Result<()> {
        self.app.restart_with(config).await
    }
}

fn project_id(project: &Value) -> Result<String> {
    Ok(project["id"].as_str().ok_or("project id")?.to_owned())
}

/// A read-only member's write: the harness returns the tool's 403 to the
/// model and the turn ends. (The removed API runtime failed the turn.)
fn write_refused(turn: &Value) -> bool {
    turn["status"] == "done" && reply(turn).contains("Workspace does not have write permission.")
}

/// Folderless conversations run in an empty, read-only managed folder (P5),
/// so the read finds no file and never reaches the host workspace. The
/// harness returns the tool error to the model and the turn ends; the
/// removed API runtime failed the turn.
async fn no_host_folder(f: &Projects, path: &str) -> Result<()> {
    let turn = f.send(path, "READ").await?;
    ensure!(turn["status"] == "done", "{path}: {turn}");
    let content: String = serde_json::from_str(reply(&turn))
        .map_err(|error| format!("{path}: reply {}: {error}", reply(&turn)))?;
    let result: Value = serde_json::from_str(&content).unwrap_or(Value::Null);
    ensure!(
        result["error"].is_string() && !content.contains("project bytes"),
        "{path}: read {content}"
    );
    Ok(())
}

fn reply(turn: &Value) -> &str {
    turn["reply"].as_str().unwrap_or("")
}

fn git(directory: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()?;
    if !output.status.success() {
        return Err(format!("git {args:?}: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// A project without its derived `codegraphIndex` status.
fn configuration(project: &Value) -> Value {
    let mut project = project.clone();
    if let Some(fields) = project.as_object_mut() {
        fields.remove("codegraphIndex");
    }
    project
}

fn configurations(projects: &Value) -> Vec<Value> {
    projects["projects"]
        .as_array()
        .map(|rows| rows.iter().map(configuration).collect())
        .unwrap_or_default()
}

fn find_project(projects: &Value, check: impl Fn(&Value) -> bool) -> Option<Value> {
    projects["projects"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| check(row)).cloned())
}

#[tokio::test]
async fn rust_project_folders_reject_system_traversal_home_credential_data_and_symlink_paths_before_creation(
) -> Result<()> {
    let mut f = Projects::start().await?;
    let safe = f.directory.join("safe");
    let escape = f.directory.join("escape");
    let alias = f.directory.join("alias");
    std::fs::create_dir(&safe)?;
    std::os::unix::fs::symlink("/etc", &escape)?;
    std::os::unix::fs::symlink(&safe, &alias)?;
    let home = f.directory.clone();
    let data = f.data()?;
    let mut paths: Vec<String> = ["/", "/etc", "/tmp"]
        .iter()
        .map(|path| (*path).to_owned())
        .collect();
    paths.push(home.to_string_lossy().into_owned());
    for name in [
        ".ssh",
        ".codex",
        ".codex-linc",
        ".claude",
        ".hermes",
        ".config/minarbot",
    ] {
        paths.push(home.join(name).to_string_lossy().into_owned());
    }
    for path in [
        data.clone(),
        data.join("sub"),
        escape.clone(),
        alias.clone(),
    ] {
        paths.push(path.to_string_lossy().into_owned());
    }
    paths.push(format!("{}/../safe", safe.to_string_lossy()));
    for path in &paths {
        f.request(
            "/api/projects",
            Some(json!({"id": new_id(), "name": "Bad",
                "folders": [{"mode": "existing", "path": path}]})),
            400,
        )
        .await
        .map_err(|error| format!("{path}: {error}"))?;
    }
    let config = f
        .config
        .clone()
        .option("projectRoot", escape.to_string_lossy());
    f.restart(config).await?;
    f.request(
        "/api/projects",
        Some(json!({"id": new_id(), "name": "never-created", "folders": [{"mode": "new"}]})),
        400,
    )
    .await?;
    ensure!(
        f.request("/api/projects", None, 200).await?["projects"]
            .as_array()
            .map(Vec::len)
            == Some(1),
        "a rejected project was created"
    );
    let browser = f
        .request(
            &format!(
                "/api/project-folders?path={}",
                f.directory.to_string_lossy().replace('/', "%2F")
            ),
            None,
            200,
        )
        .await?;
    let names: Vec<&str> = browser["folders"]
        .as_array()
        .map(|rows| rows.iter().filter_map(|row| row["name"].as_str()).collect())
        .unwrap_or_default();
    ensure!(names.contains(&"safe"), "browser: {names:?}");
    ensure!(
        !names
            .iter()
            .any(|name| ["escape", "alias", "data"].contains(name)),
        "browser lists protected folders: {names:?}"
    );
    Ok(())
}

#[tokio::test]
async fn rust_project_crud_preserves_folder_ownership_revision_fences_exact_replay_and_files_on_delete(
) -> Result<()> {
    let mut f = Projects::start().await?;
    let body = json!({"id": new_id(), "name": "Build docs", "description": "Local project",
        "folders": [{"mode": "new"}, {"mode": "existing", "path": f.workspace}],
        "members": [{"agent": f.agent, "write": true}], "confirmWrite": true});
    let mut unconfirmed = body.clone();
    unconfirmed["confirmWrite"] = json!(false);
    f.request("/api/projects", Some(unconfirmed), 400).await?;
    let p = f.request("/api/projects", Some(body.clone()), 200).await?;
    let id = project_id(&p)?;
    let path = format!("/api/projects/{id}");
    ensure!(
        p["folders"].as_array().map(Vec::len) == Some(2),
        "folders: {}",
        p["folders"]
    );
    let created = PathBuf::from(p["folders"][0].as_str().ok_or("new folder")?);
    ensure!(created.exists(), "new folder missing");
    ensure!(
        git(&created, &["rev-parse", "--show-toplevel"])? == created.to_string_lossy(),
        "new folder is not its own repository"
    );
    ensure!(
        git(&created, &["log", "--format=%s"])? == "Start project",
        "initial commit"
    );
    ensure!(
        !created.join(".git/hooks/pre-commit.sample").exists(),
        "sample hooks copied"
    );
    ensure!(
        !f.workspace.join(".git").exists(),
        "existing folder initialized"
    );
    std::fs::write(created.join("keep.txt"), "keep")?;
    ensure!(
        f.request("/api/projects", Some(body.clone()), 200).await? == p,
        "create replay differs"
    );
    let mut other = body.clone();
    other["name"] = json!("Other");
    f.request("/api/projects", Some(other), 409).await?;
    ensure!(p["members"] == body["members"], "members: {}", p["members"]);

    let edit = json!({"id": new_id(), "revision": 1, "name": "Renamed",
        "members": [{"agent": f.agent, "write": false}]});
    let changed = f
        .request(&format!("{path}/edit"), Some(edit.clone()), 200)
        .await?;
    ensure!(changed["revision"] == 2, "edit: {changed}");
    ensure!(changed["name"] == "Renamed", "edit: {changed}");
    ensure!(
        changed["sections"][0]["name"] == "general",
        "edit: {changed}"
    );
    ensure!(changed["members"] == edit["members"], "edit: {changed}");
    ensure!(
        f.request(&format!("{path}/edit"), Some(edit.clone()), 200)
            .await?
            == changed,
        "edit replay differs"
    );
    f.request(
        &format!("{path}/edit"),
        Some(json!({"id": new_id(), "revision": 1, "name": "Stale"})),
        409,
    )
    .await?;
    std::fs::remove_dir_all(&f.workspace)?;
    ensure!(
        f.request("/api/projects", Some(body.clone()), 200).await? == p,
        "replay after folder removal"
    );
    let reordered = json!({"members": edit["members"], "name": edit["name"],
        "revision": edit["revision"], "id": edit["id"]});
    ensure!(
        f.request(&format!("{path}/edit"), Some(reordered), 200)
            .await?
            == changed,
        "edit replay with reordered keys"
    );
    let config = f.config.clone();
    f.restart(config).await?;
    ensure!(
        f.request("/api/projects", Some(body), 200).await? == p,
        "replay after restart"
    );
    let remove = json!({"id": new_id(), "revision": 2, "confirmed": true});
    for _ in 0..2 {
        ensure!(
            f.request(&format!("{path}/delete"), Some(remove.clone()), 200)
                .await?
                == json!({"id": id, "deleted": true}),
            "delete receipt"
        );
    }
    ensure!(
        std::fs::read_to_string(created.join("keep.txt"))? == "keep",
        "delete removed files"
    );
    ensure!(
        find_project(&f.request("/api/projects", None, 200).await?, |row| row
            ["id"]
            == id.as_str())
        .is_none(),
        "deleted project listed"
    );
    f.request(&path, None, 404).await?;
    let db = Inspector::open(f.app.database())?;
    ensure!(
        db.scalar::<i64>(
            "SELECT archived FROM channels WHERE id=?",
            [p["channel"].as_str()]
        )? == 1,
        "project channel not archived"
    );
    ensure!(
        db.scalar::<i64>(
            "SELECT count(*) FROM channel_members WHERE channel=?",
            [p["channel"].as_str()]
        )? == 0,
        "project channel kept members"
    );
    Ok(())
}

#[tokio::test]
async fn rust_project_grants_resolve_by_room_or_selected_side_thread_folder_and_fail_closed_after_revocation(
) -> Result<()> {
    let f = Projects::start().await?;
    let other = f.directory.join("other");
    std::fs::create_dir(&other)?;
    std::fs::write(other.join("read.txt"), "second folder content")?;
    let p = f
        .request(
            "/api/projects",
            Some(json!({"id": new_id(), "name": "Scope",
                "folders": [{"mode": "existing", "path": f.workspace},
                    {"mode": "existing", "path": other}],
                "members": [{"agent": f.agent, "write": false}]})),
            200,
        )
        .await?;
    let id = project_id(&p)?;
    let direct = f
        .send(&format!("/api/agents/{}/turns", f.agent), "READ")
        .await?;
    ensure!(direct["status"] == "done", "direct: {direct}");
    ensure!(
        !reply(&direct).contains("project bytes")
            && !reply(&direct).contains("second folder content"),
        "direct chat read a project folder: {}",
        reply(&direct)
    );
    let room = f
        .send(
            &format!(
                "/api/channels/{}/turns",
                p["channel"].as_str().ok_or("channel")?
            ),
            "READ",
        )
        .await?;
    ensure!(
        reply(&room).contains("project bytes"),
        "room: {}",
        reply(&room)
    );
    let side = f
        .request(
            &format!("/api/projects/{id}/thread"),
            Some(
                json!({"id": new_id(), "revision": 1, "agent": f.agent, "folder": other,
                "name": "Second folder"}),
            ),
            200,
        )
        .await?;
    let path = format!(
        "/api/threads/{}/turns",
        side["threads"][0]["id"].as_str().ok_or("side thread")?
    );
    let read = f.send(&path, "READ").await?;
    ensure!(
        reply(&read).contains("second folder content"),
        "side: {}",
        reply(&read)
    );
    ensure!(
        write_refused(&f.send(&path, "WRITE").await?),
        "read-only write"
    );
    ensure!(
        !other.join("made.txt").exists(),
        "read-only write created a file"
    );
    f.request(
        &format!("/api/projects/{id}/edit"),
        Some(json!({"id": new_id(), "revision": 2, "members": []})),
        200,
    )
    .await?;
    // Revocation fails closed at admission: since #87 every send resolves
    // its harness workspace before it is queued.
    let revoked = f
        .request(&path, Some(json!({"id": new_id(), "text": "READ"})), 403)
        .await?;
    ensure!(
        revoked["error"]
            .as_str()
            .is_some_and(|error| error.to_lowercase().contains("not a project member")),
        "revoked error: {revoked}"
    );
    Ok(())
}

#[tokio::test]
async fn built_in_codegraph_indexes_projects_and_serves_api_turns_without_a_cli() -> Result<()> {
    let mut f = Projects::start().await?;
    let projects = f.request("/api/projects", None, 200).await?;
    ensure!(
        projects["codegraphAvailable"] == true,
        "CodeGraph needs Linux with bubblewrap: {}",
        projects["codegraphAvailable"]
    );
    let source = f.workspace.join("fixture.ts");
    std::fs::write(&source, "export function projectFixture() { return 42; }\n")?;
    std::fs::write(
        f.directory.join("outside.ts"),
        "export function outsideFixture() {}\n",
    )?;
    std::os::unix::fs::symlink(
        f.directory.join("outside.ts"),
        f.workspace.join("outside.ts"),
    )?;
    let id = project_id(&f.project)?;
    let path = format!("/api/projects/{id}");
    ensure!(
        f.project["codegraphEnabled"] == true,
        "project: {}",
        f.project
    );
    // A fresh folder proves indexing happened before any turn.
    let second = f.directory.join("codegraph-source");
    std::fs::create_dir(&second)?;
    std::fs::write(
        second.join("second.ts"),
        "export const secondFixture = true;\n",
    )?;
    let changed = f
        .request(
            &format!("{path}/edit"),
            Some(json!({"id": new_id(), "revision": f.project["revision"],
                "folders": [f.workspace, second]})),
            200,
        )
        .await?;
    let status = minarbot_testkit::wait::until_within(
        "CodeGraph index leaves queued",
        Duration::from_secs(15),
        || async {
            let status = f.request(&path, None, 200).await?["codegraphIndex"].clone();
            Ok((status["state"] != "queued").then_some(status))
        },
    )
    .await?;
    ensure!(status["state"] == "ready", "index: {status}");
    ensure!(
        status["files"].as_i64().is_some_and(|files| files >= 1),
        "index: {status}"
    );
    let turns = format!(
        "/api/channels/{}/turns",
        f.project["channel"].as_str().ok_or("channel")?
    );
    let first = f.send(&turns, "EXPLORE").await?;
    ensure!(first["status"] == "done", "explore: {}", first["error"]);
    ensure!(
        reply(&first).contains("projectFixture() { return 42; }"),
        "explore: {}",
        reply(&first)
    );
    ensure!(
        !reply(&first).contains("outsideFixture"),
        "symlink followed"
    );
    ensure!(
        !f.workspace.join(".codegraph").exists(),
        "index written into the folder"
    );
    std::fs::write(&source, "export function projectFixture() { return 43; }\n")?;
    ensure!(
        reply(&f.send(&turns, "EXPLORE").await?).contains("return 43"),
        "index not refreshed"
    );
    let legacy = json!({"id": new_id(), "revision": changed["revision"]});
    let mut disable = legacy.clone();
    disable["codegraphEnabled"] = json!(false);
    let enabled = f
        .request(&format!("{path}/edit"), Some(disable), 200)
        .await?;
    ensure!(
        enabled["codegraphEnabled"] == true,
        "CodeGraph disabled: {enabled}"
    );
    ensure!(
        f.request(&format!("{path}/edit"), Some(legacy.clone()), 200)
            .await?["revision"]
            == enabled["revision"],
        "legacy replay revision"
    );
    ensure!(
        f.request(&path, None, 200).await?["codegraphIndex"]["state"] == "ready",
        "index state"
    );
    ensure!(
        reply(&f.send(&turns, "EXPLORE").await?).contains("return 43"),
        "explore after legacy edit"
    );
    let legacy_id = legacy["id"].clone();
    f.app
        .edit_database(|db| {
            db.execute(
                "UPDATE local_projects SET codegraph_enabled=0 WHERE id=?",
                [&id],
            )?;
            db.execute(
                "UPDATE local_project_commands SET request=json_set(request,'$.codegraphEnabled',json('false')), result=json_set(result,'$.codegraphEnabled',json('false')) WHERE id=?",
                [legacy_id.as_str()],
            )?;
            Ok(())
        })
        .await?;
    let config = f.config.clone();
    f.restart(config).await?;
    ensure!(
        f.request(&format!("{path}/edit"), Some(legacy), 200)
            .await?["codegraphEnabled"]
            == true,
        "legacy disabled state survived"
    );
    ensure!(
        reply(&f.send(&turns, "EXPLORE").await?).contains("return 43"),
        "explore after restart"
    );
    Ok(())
}

#[tokio::test]
async fn project_http_owner_rules_direct_chat_isolation_read_write_enforcement_env_fallback_per_folder_git_and_restart_receipts(
) -> Result<()> {
    let mut f = Projects::start().await?;
    let client = f.app.client()?;
    let projects_url = f.app.url("/api/projects");
    let peer = client
        .get(&projects_url)
        .header("Origin", f.app.origin())
        .bearer_auth("peer")
        .send()
        .await?;
    ensure!(
        peer.status().as_u16() == 403,
        "peer bearer: {}",
        peer.status()
    );
    let foreign = client
        .get(&projects_url)
        .header("Origin", "https://foreign.invalid")
        .send()
        .await?;
    ensure!(
        foreign.status().as_u16() == 403,
        "foreign origin: {}",
        foreign.status()
    );
    let plain = client
        .post(&projects_url)
        .header("Origin", f.app.origin())
        .header("Content-Type", "text/plain")
        .body(json!({"id": new_id(), "name": "Rejected", "folders": [{"mode": "new"}]}).to_string())
        .send()
        .await?;
    ensure!(
        plain.status().as_u16() == 403,
        "text/plain: {}",
        plain.status()
    );
    let host = client
        .get(&projects_url)
        .header("Host", "foreign.invalid")
        .send()
        .await?;
    ensure!(
        host.status().as_u16() == 403,
        "foreign host: {}",
        host.status()
    );

    let direct = format!("/api/agents/{}/turns", f.agent);
    let room = format!(
        "/api/channels/{}/turns",
        f.project["channel"].as_str().ok_or("channel")?
    );
    let id = project_id(&f.project)?;
    let managed = f.data()?.join("agent-workspaces").join(&f.agent);
    std::fs::write(managed.join("read.txt"), "agent workspace bytes")?;
    let direct_read = f.send(&direct, "READ").await?;
    ensure!(
        direct_read["status"] == "done",
        "direct: {}",
        direct_read["error"]
    );
    ensure!(
        reply(&direct_read).contains("agent workspace bytes"),
        "direct: {}",
        reply(&direct_read)
    );
    ensure!(
        !reply(&direct_read).contains("project bytes"),
        "direct read the project"
    );
    ensure!(
        f.model.requests().last().is_some_and(|request| request
            .tool_names()
            .iter()
            .any(|name| name == "read_workspace_file")),
        "read_workspace_file not offered"
    );
    let read = f.send(&room, "READ").await?;
    ensure!(read["status"] == "done", "room read: {}", read["error"]);
    ensure!(
        reply(&read).contains("project bytes"),
        "room read: {}",
        reply(&read)
    );
    ensure!(
        write_refused(&f.send(&room, "WRITE").await?),
        "read-only write"
    );
    ensure!(
        !f.workspace.join("made.txt").exists(),
        "read-only write created a file"
    );
    let body = json!({"id": new_id(), "revision": 1, "members": [{"agent": f.agent, "write": true}],
        "confirmWrite": true});
    let edited = f
        .request(&format!("/api/projects/{id}/edit"), Some(body.clone()), 200)
        .await?;
    ensure!(edited["revision"] == 2, "edit: {edited}");
    ensure!(
        f.send(&room, "WRITE").await?["status"] == "done",
        "write after grant"
    );
    ensure!(
        std::fs::read_to_string(f.workspace.join("made.txt"))? == "made",
        "made.txt"
    );
    git(&f.workspace, &["init", "-q"])?;
    // The gateway peer keeps its protocol log and session in its cwd; keep
    // them out of the project's Git status like a real harness's state.
    std::fs::write(
        f.workspace.join(".git/info/exclude"),
        "fake-gateway-log.jsonl\n.fake-gateway-*.json\n",
    )?;
    let status = f
        .request(&format!("/api/projects/{id}/git"), None, 200)
        .await?;
    ensure!(status["isRepo"] == true, "git: {status}");
    ensure!(status["dirtyCount"] == 2, "git: {status}");
    ensure!(f.send(&room, "GIT").await?["status"] == "done", "git turn");
    let second = f.directory.join("second-repo");
    std::fs::create_dir(&second)?;
    git(&second, &["init", "-q"])?;
    std::fs::write(
        second.join(".git/info/exclude"),
        "fake-gateway-log.jsonl\n.fake-gateway-*.json\n",
    )?;
    std::fs::write(second.join("second.txt"), "second")?;
    let multi = f
        .request(
            "/api/projects",
            Some(json!({"id": new_id(), "name": "Multiple repositories",
                "folders": [{"mode": "existing", "path": f.workspace},
                    {"mode": "existing", "path": second}],
                "members": [{"agent": f.agent, "write": false}]})),
            200,
        )
        .await?;
    let multi_id = project_id(&multi)?;
    let other_git = f
        .request(
            &format!(
                "/api/projects/{multi_id}/git?folder={}",
                second.to_string_lossy().replace('/', "%2F")
            ),
            None,
            200,
        )
        .await?;
    ensure!(other_git["dirtyCount"] == 1, "second repo: {other_git}");
    ensure!(
        other_git["changedFiles"][0]["path"] == "second.txt",
        "second repo: {other_git}"
    );
    let side = f
        .request(
            &format!("/api/projects/{multi_id}/thread"),
            Some(
                json!({"id": new_id(), "revision": 1, "agent": f.agent, "folder": second,
                "name": "Second repository"}),
            ),
            200,
        )
        .await?;
    let git_turn = f
        .send(
            &format!(
                "/api/threads/{}/turns",
                side["threads"][0]["id"].as_str().ok_or("thread")?
            ),
            "GIT",
        )
        .await?;
    ensure!(
        git_turn["status"] == "done",
        "side git: {}",
        git_turn["error"]
    );
    ensure!(
        reply(&git_turn).contains("second.txt"),
        "side git: {}",
        reply(&git_turn)
    );
    f.request(
        &format!("/api/projects/{multi_id}/delete"),
        Some(json!({"id": new_id(), "revision": 2, "confirmed": true})),
        200,
    )
    .await?;
    ensure!(
        second.join("second.txt").exists(),
        "delete removed repository files"
    );
    let config = f.config.clone();
    f.restart(config).await?;
    ensure!(
        f.request(&format!("/api/projects/{id}/edit"), Some(body), 200)
            .await?
            == edited,
        "edit receipt after restart"
    );
    ensure!(
        f.request("/api/projects", None, 200).await?["projects"]
            .as_array()
            .map(Vec::len)
            == Some(1),
        "project count"
    );
    let fallback = f.config.clone().workspace(&f.agent, &f.workspace, false);
    f.restart(fallback).await?;
    let env_read = f.send(&direct, "READ").await?;
    ensure!(
        env_read["status"] == "done",
        "env fallback: {}",
        env_read["error"]
    );
    ensure!(
        reply(&env_read).contains("project bytes"),
        "env fallback: {}",
        reply(&env_read)
    );
    let deleted = f
        .request(
            &format!("/api/projects/{id}/delete"),
            Some(json!({"id": new_id(), "revision": 2, "confirmed": true})),
            200,
        )
        .await?;
    ensure!(deleted["deleted"] == true, "delete: {deleted}");
    ensure!(
        f.workspace.join("made.txt").exists(),
        "delete removed files"
    );
    ensure!(
        f.request("/api/projects", None, 200).await?["projects"]
            .as_array()
            .map(Vec::len)
            == Some(0),
        "deleted project listed"
    );
    ensure!(
        f.request(&room, None, 200)
            .await?
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["id"] == read["id"])),
        "room history lost"
    );
    Ok(())
}

#[tokio::test]
async fn project_harness_cwd_and_read_only_native_escalation_denial_direct_chat_uses_the_managed_folder(
) -> Result<()> {
    let mut f = Projects::start().await?;
    let bin = f.directory.join("bin");
    std::fs::create_dir_all(&bin)?;
    let codex = fakes::install(Fake::Codex, &bin, "fake-codex")?;
    let claude = fakes::install(Fake::Claude, &bin, "fake-claude")?;
    let issuer = SessionIssuer::start().await?;
    // Native peers answer these turns, so the gateway (and its model) is not configured.
    let config = ServerConfig::new()
        .option(
            "projectRoot",
            f.directory.join("Projects").to_string_lossy(),
        )
        .env("HOME", f.directory.to_string_lossy())
        .executable("codex", &codex)
        .executable("claude", &claude)
        .issuer(&issuer);
    f.restart(config).await?;
    // Once a gateway was configured, the test-mode `runtime: codex` shortcut
    // keeps meaning the gateway instance, so bind explicit fake instances.
    create_instance(&f.app, "fake-codex", "codex", &codex).await?;
    create_instance(&f.app, "fake-claude", "claude", &claude).await?;
    let profile = |revision: i64, runtime: &str| {
        json!({"id": new_id(), "revision": revision, "name": "Coder", "model": "fixture",
            "instructions": "Help", "providerInstance": format!("fake-{runtime}")})
    };
    f.request(
        &format!("/api/agents/{}/edit", f.agent),
        Some(profile(1, "codex")),
        200,
    )
    .await?;
    let room = format!(
        "/api/channels/{}/turns",
        f.project["channel"].as_str().ok_or("channel")?
    );
    let managed_log = f
        .data()?
        .join("agent-workspaces")
        .join(&f.agent)
        .join("fake-log.jsonl");
    for runtime in ["codex", "claude"] {
        if runtime == "claude" {
            f.request(
                &format!("/api/agents/{}/edit", f.agent),
                Some(profile(2, runtime)),
                200,
            )
            .await?;
        }
        let marker = if runtime == "codex" {
            "thread/start"
        } else {
            "\"type\":\"init\""
        };
        let done = f.send(&room, "READ_ONLY_WRITE").await?;
        ensure!(done["status"] == "done", "{runtime}: {}", done["error"]);
        ensure!(
            reply(&done) == "denied",
            "{runtime}: reply {}",
            reply(&done)
        );
        let log = f.workspace.join("fake-log.jsonl");
        ensure!(
            log.exists(),
            "{runtime}: harness did not run in the project folder"
        );
        ensure!(
            std::fs::read_to_string(&log)?.contains(marker),
            "{runtime}: project log lacks {marker}"
        );
        ensure!(
            done["harnessApprovals"].as_array().map(Vec::len) == Some(0),
            "{runtime}: approvals {}",
            done["harnessApprovals"]
        );
        let direct = f
            .send(&format!("/api/agents/{}/turns", f.agent), "Direct chat")
            .await?;
        ensure!(
            direct["status"] == "done",
            "{runtime}: direct {}",
            direct["error"]
        );
        ensure!(
            std::fs::read_to_string(&managed_log)?.contains(marker),
            "{runtime}: direct chat did not use the managed folder"
        );
    }
    Ok(())
}

#[tokio::test]
async fn rust_managed_folder_retry_recovers_durable_intent_after_transaction_failure_without_deleting_files(
) -> Result<()> {
    let f = Projects::start().await?;
    let db = Fixture::open(f.app.database())?;
    let body = json!({"id": new_id(), "name": "Retry folder", "folders": [{"mode": "new"}]});
    db.execute_batch(
        "CREATE TRIGGER reject_project BEFORE INSERT ON local_projects BEGIN SELECT RAISE(ABORT,'fixture failure'); END;",
    )?;
    f.request("/api/projects", Some(body.clone()), 500).await?;
    let folder = f.directory.join("Projects").join("retry-folder");
    ensure!(
        folder.exists(),
        "managed folder not created before the failure"
    );
    std::fs::write(folder.join("retained.txt"), "retained")?;
    db.execute_batch("DROP TRIGGER reject_project")?;
    let p = f.request("/api/projects", Some(body.clone()), 200).await?;
    ensure!(
        p["folders"][0] == folder.to_string_lossy().as_ref(),
        "folder: {}",
        p["folders"]
    );
    ensure!(
        std::fs::read_to_string(folder.join("retained.txt"))? == "retained",
        "retry deleted files"
    );
    ensure!(
        f.request("/api/projects", Some(body), 200).await? == p,
        "create replay differs"
    );
    Ok(())
}

#[tokio::test]
async fn standalone_and_p2_migration_preserves_channel_ids_history_read_state_memory_files_and_legacy_sidebar_through_restart_and_backup_restore(
) -> Result<()> {
    let mut f = Projects::start().await?;
    let (room, turn, file) = (new_id(), new_id(), new_id());
    let layout = f.request("/api/sidebar", None, 200).await?;
    let legacy = f
        .request(
            "/api/sidebar",
            Some(json!({"id": new_id(), "revision": layout["revision"],
                "sections": [{"id": new_id(), "name": "Legacy labels", "agents": [f.agent]}]})),
            200,
        )
        .await?;
    f.app.shutdown().await?;
    let legacy_file = f.directory.join("legacy-snapshot.sqlite");
    let (retained_channel, retained_turn, retained_file) = {
        // Recreate pre-migration standalone and P2 data in their historical schema.
        let db = Fixture::open(f.app.database())?;
        db.execute_batch("DROP TABLE local_project_sections")?;
        db.execute(
            "INSERT INTO channels(id,name,instructions,lead,activity,read_position) VALUES (?,?,?,?,12,9)",
            (&room, "Acme", "Plan Acme", &f.agent),
        )?;
        db.execute(
            "INSERT INTO channel_members VALUES (?,?)",
            (&room, &f.agent),
        )?;
        db.execute(
            "INSERT INTO turns(id,agent,channel,text,reply,status,created) VALUES (?,?,?,?,?,'done','2026-11-14T00:00:00.000Z')",
            (&turn, &f.agent, &room, "Retained brief", "Retained answer"),
        )?;
        db.execute(
            "INSERT INTO files(id,scope,name,digest,bytes,created) VALUES (?,?,?,?,?,'2026-11-14T00:00:00.000Z')",
            (&file, format!("channels:{room}"), "retained.txt", "fixture", b"retained bytes".to_vec()),
        )?;
        db.execute(
            "UPDATE channels SET activity=12,read_position=9 WHERE id=?",
            [&room],
        )?;
        db.execute(
            "INSERT INTO memories(scope,id,text,essential,revision,removed) VALUES (?,?,?,1,1,0)",
            (format!("channels:{room}"), new_id(), "Retained team memory"),
        )?;
        let rows = (
            db.row("SELECT * FROM channels WHERE id=?", [&room])?,
            db.row("SELECT * FROM turns WHERE id=?", [&turn])?,
            db.row("SELECT * FROM files WHERE id=?", [&file])?,
        );
        drop(db);
        backup::workspace_snapshot(&f.app.database(), &legacy_file)?;
        rows
    };
    let config = f.config.clone();
    f.app.start_stopped(config).await?;
    let projects = f.request("/api/projects", None, 200).await?;
    let migrated = find_project(&projects, |p| p["name"] == "Acme").ok_or("migrated project")?;
    ensure!(migrated["channel"] == room.as_str(), "migrated: {migrated}");
    ensure!(
        migrated["sections"][0]["id"] == room.as_str(),
        "migrated: {migrated}"
    );
    ensure!(
        migrated["sections"][0]["name"] == "general",
        "migrated: {migrated}"
    );
    ensure!(
        migrated["sections"][0]["activity"] == 12,
        "migrated: {migrated}"
    );
    ensure!(
        migrated["sections"][0]["read_position"] == 9,
        "migrated: {migrated}"
    );
    ensure!(migrated["folders"] == json!([]), "migrated: {migrated}");
    ensure!(
        migrated["members"] == json!([{"agent": f.agent, "write": false}]),
        "migrated: {migrated}"
    );
    let p2 = find_project(&projects, |p| p["id"] == f.project["id"]).ok_or("P2 project")?;
    ensure!(p2["channel"] == f.project["channel"], "p2: {p2}");
    ensure!(p2["sections"][0]["name"] == "general", "p2: {p2}");
    ensure!(p2["folders"] == f.project["folders"], "p2: {p2}");
    ensure!(p2["members"] == f.project["members"], "p2: {p2}");
    let saved_memory = f
        .request(&format!("/api/channels/{room}/memory"), None, 200)
        .await?;
    ensure!(
        f.request("/api/sidebar", None, 200).await? == legacy,
        "legacy sidebar"
    );
    let config = f.config.clone();
    f.restart(config).await?;
    let restarted = f.request("/api/projects", None, 200).await?;
    ensure!(
        configurations(&restarted) == configurations(&projects),
        "restart changed projects"
    );
    f.app.shutdown().await?;
    let snapshot_file = f.directory.join("snapshot.sqlite");
    {
        let source = Fixture::open(f.app.database())?;
        ensure!(
            source.row("SELECT * FROM turns WHERE id=?", [&turn])? == retained_turn,
            "turn row changed"
        );
        ensure!(
            source.row("SELECT * FROM files WHERE id=?", [&file])? == retained_file,
            "file row changed"
        );
        let mut actual = source.row("SELECT * FROM channels WHERE id=?", [&room])?;
        actual["project"] = Value::Null;
        let mut expected = retained_channel.clone();
        expected["project"] = Value::Null;
        ensure!(
            actual == expected,
            "channel row changed: {actual} vs {expected}"
        );
    }
    backup::workspace_snapshot(&f.app.database(), &snapshot_file)?;
    let expected_projects = configurations(&projects);
    let Projects { app, config, .. } = f;
    // Keep the first root (and the snapshots in it) while freeing its slot.
    let _first_root = app.root().clone();
    drop(app);
    for (file, current) in [(&snapshot_file, true), (&legacy_file, false)] {
        let root = TestRoot::new()?;
        backup::restore_workspace(file, &root.database()).await?;
        let restored = root.start(config.clone()).await?;
        let upgraded = restored.get("/api/projects").await?;
        if current {
            ensure!(
                configurations(&upgraded) == expected_projects,
                "restored projects differ"
            );
            let history = restored.get(&format!("/api/channels/{room}/turns")).await?;
            ensure!(
                history[0]["id"] == turn.as_str(),
                "restored history: {history}"
            );
            ensure!(
                history[0]["reply"] == "Retained answer",
                "restored history: {history}"
            );
            let bytes = restored
                .client()?
                .get(restored.url(&format!(
                    "/api/channels/{room}/files/{file_id}",
                    file_id = file_name(&retained_file)
                )))
                .send()
                .await?
                .text()
                .await?;
            ensure!(bytes == "retained bytes", "restored file: {bytes}");
        } else {
            ensure!(
                find_project(&upgraded, |p| p["name"] == "Acme")
                    .is_some_and(|p| p["sections"][0]["id"] == room.as_str()),
                "legacy restore section id"
            );
            ensure!(
                find_project(&upgraded, |p| p["id"]
                    == expected_projects
                        .iter()
                        .find(|q| q["name"] == "Project")
                        .map_or(Value::Null, |q| q["id"].clone()))
                .is_some_and(|p| p["sections"][0]["id"] == p["channel"]),
                "legacy restore P2 section id"
            );
            ensure!(
                restored.get(&format!("/api/channels/{room}/turns")).await?[0]["id"]
                    == turn.as_str(),
                "legacy restore history"
            );
        }
        ensure!(
            restored
                .get(&format!("/api/channels/{room}/memory"))
                .await?
                == saved_memory,
            "restored memory"
        );
        ensure!(
            restored.get("/api/sidebar").await? == legacy,
            "restored sidebar"
        );
    }
    Ok(())
}

fn file_name(row: &Value) -> String {
    row["id"].as_str().unwrap_or("").to_owned()
}

#[tokio::test]
async fn section_commands_require_revisions_and_request_ids_inherit_members_reorder_archive_and_replay_across_restart(
) -> Result<()> {
    let mut f = Projects::start().await?;
    let id = project_id(&f.project)?;
    let channel = f.project["channel"].as_str().ok_or("channel")?.to_owned();
    let path = format!("/api/projects/{id}/sections");
    f.request(
        &path,
        Some(json!({"id": new_id(), "revision": 1, "name": "general"})),
        409,
    )
    .await?;
    let peer = f
        .app
        .client()?
        .post(f.app.url(&path))
        .header("Origin", f.app.origin())
        .header("Content-Type", "application/json")
        .bearer_auth("peer")
        .body(json!({"id": new_id(), "revision": 1, "name": "design"}).to_string())
        .send()
        .await?;
    ensure!(peer.status().as_u16() == 403, "peer: {}", peer.status());
    f.request(
        &path,
        Some(json!({"id": new_id(), "revision": 1, "name": "design", "members": []})),
        400,
    )
    .await?;
    let command = json!({"id": new_id(), "revision": 1, "name": "design"});
    let created = f.request(&path, Some(command.clone()), 201).await?;
    let design = created["sections"]
        .as_array()
        .and_then(|sections| sections.iter().find(|s| s["name"] == "design"))
        .cloned()
        .ok_or("design section")?;
    let design_id = design["id"].as_str().ok_or("design id")?.to_owned();
    ensure!(created["revision"] == 2, "created: {created}");
    ensure!(design["revision"] == 1, "design: {design}");
    ensure!(design_id != channel, "design reused the general channel");
    let channels = f.request("/api/channels", None, 200).await?;
    let members_of = |channels: &Value, id: &str| {
        channels
            .as_array()
            .and_then(|rows| rows.iter().find(|c| c["id"] == id))
            .map(|c| c["members"].clone())
    };
    ensure!(
        members_of(&channels, &design_id) == Some(json!([f.agent])),
        "design members"
    );
    ensure!(
        f.request(&path, Some(command.clone()), 201).await? == created,
        "section replay"
    );
    let mut renamed = command.clone();
    renamed["name"] = json!("backend");
    f.request(&path, Some(renamed), 409).await?;
    f.request(
        &path,
        Some(json!({"id": new_id(), "revision": 1, "name": "backend"})),
        409,
    )
    .await?;
    let edit_path = format!("{path}/{design_id}/edit");
    let edit = json!({"id": new_id(), "revision": 2, "name": "frontend", "position": 0});
    let reordered = f.request(&edit_path, Some(edit.clone()), 200).await?;
    let names: Vec<&str> = reordered["sections"]
        .as_array()
        .map(|rows| rows.iter().filter_map(|s| s["name"].as_str()).collect())
        .unwrap_or_default();
    ensure!(names == ["frontend", "general"], "order: {names:?}");
    ensure!(
        reordered["sections"][0]["revision"] == 2,
        "revisions: {reordered}"
    );
    ensure!(
        reordered["sections"][1]["revision"] == 2,
        "revisions: {reordered}"
    );
    f.request(
        &edit_path,
        Some(json!({"id": new_id(), "revision": 3, "archived": "yes"})),
        400,
    )
    .await?;
    f.request(
        &edit_path,
        Some(json!({"id": new_id(), "revision": 3, "position": "0"})),
        400,
    )
    .await?;
    f.request(
        &format!("/api/projects/{}/sections/{design_id}/edit", new_id()),
        Some(json!({"id": new_id(), "revision": 3, "name": "wrong"})),
        404,
    )
    .await?;
    let section_turns = format!("/api/channels/{design_id}/turns");
    let turn = f.send(&section_turns, "READ").await?;
    ensure!(turn["status"] == "done", "section read: {}", turn["error"]);
    ensure!(
        reply(&turn).contains("project bytes"),
        "section read: {}",
        reply(&turn)
    );
    ensure!(
        !turn["room"].is_null() && turn["room"] != false,
        "Section uses H2 room engine: {}",
        turn["room"]
    );
    let archived = f
        .request(
            &edit_path,
            Some(json!({"id": new_id(), "revision": 3, "archived": true})),
            200,
        )
        .await?;
    ensure!(
        archived["sections"][0]["archived"] == true,
        "archived: {archived}"
    );
    ensure!(
        f.request(&section_turns, None, 200).await?[0]["id"] == turn["id"],
        "archived history"
    );
    f.request(
        &section_turns,
        Some(json!({"id": new_id(), "text": "Cannot send"})),
        409,
    )
    .await?;
    let config = f.config.clone();
    f.restart(config).await?;
    ensure!(
        f.request(&edit_path, Some(edit), 200).await? == reordered,
        "edit replay after restart"
    );
    ensure!(
        f.request(&path, None, 200).await?[0]["archived"] == true,
        "archived after restart"
    );
    let restored = f
        .request(
            &edit_path,
            Some(json!({"id": new_id(), "revision": 4, "archived": false})),
            200,
        )
        .await?;
    ensure!(restored["revision"] == 5, "unarchive: {restored}");
    let removed = f
        .request(
            &format!("/api/projects/{id}/edit"),
            Some(json!({"id": new_id(), "revision": 5, "members": []})),
            200,
        )
        .await?;
    ensure!(removed["revision"] == 6, "remove members: {removed}");
    let channels = f.request("/api/channels", None, 200).await?;
    for section in [&design_id, &channel] {
        ensure!(
            members_of(&channels, section) == Some(json!([])),
            "section {section} kept members"
        );
    }
    Ok(())
}

#[tokio::test]
async fn folderless_project_and_side_threads_isolate_host_grants_accept_folders_later_and_archive_retains_configuration(
) -> Result<()> {
    let mut f = Projects::start().await?;
    let create = json!({"id": new_id(), "name": "No folders",
        "members": [{"agent": f.agent, "write": false}]});
    let project = f
        .request("/api/projects", Some(create.clone()), 200)
        .await?;
    let id = project_id(&project)?;
    ensure!(project["folders"] == json!([]), "folders: {project}");
    ensure!(
        project["sections"][0]["name"] == "general",
        "sections: {project}"
    );
    ensure!(
        f.request("/api/projects", Some(create.clone()), 200)
            .await?
            == project,
        "create replay"
    );
    let path = format!(
        "/api/channels/{}/turns",
        project["channel"].as_str().ok_or("channel")?
    );
    let config = f.config.clone().workspace(&f.agent, &f.workspace, true);
    f.restart(config).await?;
    ensure!(
        f.send(&format!("/api/agents/{}/turns", f.agent), "READ")
            .await?["status"]
            == "done",
        "direct read with host grant"
    );
    // No implicit host binding inside folderless sections.
    no_host_folder(&f, &path).await?;
    let project = f
        .request(
            &format!("/api/projects/{id}/thread"),
            Some(json!({"id": new_id(), "revision": 1, "agent": f.agent, "name": "Focused"})),
            200,
        )
        .await?;
    let thread = project["threads"][0].clone();
    ensure!(thread["folder"] == "", "thread folder: {thread}");
    let thread_turns = format!(
        "/api/threads/{}/turns",
        thread["id"].as_str().ok_or("thread")?
    );
    no_host_folder(&f, &thread_turns).await?;
    f.request(
        &format!("/api/projects/{id}/edit"),
        Some(json!({"id": new_id(), "revision": 2, "folders": [f.workspace]})),
        200,
    )
    .await?;
    ensure!(
        f.send(&path, "READ").await?["status"] == "done",
        "read after folder added"
    );
    ensure!(
        f.send(&thread_turns, "READ").await?["status"] == "done",
        "thread read after folder added"
    );
    let other = f
        .request(
            "/api/projects",
            Some(json!({"id": new_id(), "name": "Separate",
                "members": [{"agent": f.agent, "write": false}]})),
            200,
        )
        .await?;
    // Another folderless project does not borrow this project's folder.
    no_host_folder(
        &f,
        &format!(
            "/api/channels/{}/turns",
            other["channel"].as_str().ok_or("channel")?
        ),
    )
    .await?;
    let archive = json!({"id": new_id(), "revision": 3, "confirmed": true});
    let archived = f
        .request(
            &format!("/api/projects/{id}/archive"),
            Some(archive.clone()),
            200,
        )
        .await?;
    ensure!(archived["archived"] == true, "archive: {archived}");
    ensure!(
        archived["folders"] == json!([f.workspace]),
        "archive: {archived}"
    );
    ensure!(
        archived["members"] == create["members"],
        "archive: {archived}"
    );
    ensure!(
        find_project(&f.request("/api/projects", None, 200).await?, |p| p["id"]
            == id.as_str())
        .is_none(),
        "archived project listed"
    );
    ensure!(
        f.request(&format!("/api/projects/{id}/archive"), Some(archive), 200)
            .await?
            == archived,
        "archive replay"
    );
    f.request(
        &path,
        Some(json!({"id": new_id(), "text": "Archived"})),
        409,
    )
    .await?;
    ensure!(
        f.request(&format!("/api/projects/{id}"), None, 200).await?["threads"][0]["id"]
            == thread["id"],
        "archived threads"
    );
    ensure!(
        f.workspace.join("read.txt").exists(),
        "archive removed files"
    );
    Ok(())
}

#[tokio::test]
async fn rust_project_migration_rolls_back_ownership_and_retries_without_touching_standalone_data(
) -> Result<()> {
    let mut f = Projects::start().await?;
    f.app.shutdown().await?;
    let room = new_id();
    {
        let db = Fixture::open(f.app.database())?;
        db.execute(
            "INSERT INTO channels(id,name,instructions,lead) VALUES (?,?,?,?)",
            (&room, "Retain", "History", &f.agent),
        )?;
        db.execute(
            "INSERT INTO channel_members VALUES (?,?)",
            (&room, &f.agent),
        )?;
        db.execute_batch(
            "CREATE TRIGGER fail_migration BEFORE INSERT ON local_project_sections BEGIN SELECT RAISE(ABORT,'interrupted migration'); END",
        )?;
    }
    // The failing start reuses the stopped server's slot: a second slot
    // could deadlock behind another test's two-slot reservation.
    let failure = f
        .app
        .root()
        .start_error_beside(&f.app, f.config.clone())
        .await?;
    ensure!(
        failure.contains("Project migration failed"),
        "startup error: {failure}"
    );
    {
        let db = Fixture::open(f.app.database())?;
        ensure!(
            db.scalar::<i64>("SELECT COUNT(*) FROM local_projects", [])? == 1,
            "migration kept a partial project"
        );
        ensure!(
            db.scalar::<Option<String>>("SELECT project FROM channels WHERE id=?", [&room])?
                .is_none(),
            "migration kept channel ownership"
        );
        ensure!(
            db.scalar::<i64>(
                "SELECT count(*) FROM channel_members WHERE channel=?",
                [&room]
            )? == 1,
            "migration changed members"
        );
        db.execute_batch("DROP TRIGGER fail_migration")?;
    }
    let config = f.config.clone();
    f.app.start_stopped(config.clone()).await?;
    let before = f.request("/api/projects", None, 200).await?;
    f.restart(config).await?;
    let after = f.request("/api/projects", None, 200).await?;
    ensure!(
        configurations(&after) == configurations(&before),
        "restart changed projects"
    );
    let mut before_rest = before.clone();
    let mut after_rest = after.clone();
    before_rest["projects"] = Value::Null;
    after_rest["projects"] = Value::Null;
    ensure!(before_rest == after_rest, "project listing fields changed");
    let project = find_project(&before, |p| p["id"] == room.as_str()).ok_or("migrated project")?;
    ensure!(
        project["sections"][0]["id"] == room.as_str(),
        "migrated: {project}"
    );
    ensure!(
        project["sections"][0]["name"] == "general",
        "migrated: {project}"
    );
    Ok(())
}

#[tokio::test]
async fn rust_migrates_a_standalone_channel_with_a_colliding_id_into_its_own_project() -> Result<()>
{
    let mut f = Projects::start().await?;
    let project_id = project_id(&f.project)?;
    let agent = f.agent.clone();
    f.app
        .edit_database(|db| {
            db.execute(
                "INSERT INTO channels(id,name,instructions,lead) VALUES (?,?,?,?)",
                (&project_id, "Standalone collision", "Separate work", &agent),
            )?;
            db.execute(
                "INSERT INTO channel_members VALUES (?,?)",
                (&project_id, &agent),
            )?;
            Ok(())
        })
        .await?;
    let projects = f.request("/api/projects", None, 200).await?;
    let migrated = find_project(&projects, |p| p["name"] == "Standalone collision")
        .ok_or("migrated project")?;
    ensure!(
        migrated["id"] != project_id.as_str(),
        "migration reused the project ID"
    );
    ensure!(
        migrated["sections"][0]["id"] == project_id.as_str(),
        "migrated section: {migrated}"
    );
    let original =
        find_project(&projects, |p| p["id"] == project_id.as_str()).ok_or("original project")?;
    ensure!(
        configuration(&original) == f.project,
        "original project changed: {original} vs {}",
        f.project
    );
    let config = f.config.clone();
    f.restart(config).await?;
    let after = f.request("/api/projects", None, 200).await?;
    ensure!(
        configurations(&after) == configurations(&projects),
        "restart changed projects"
    );
    Ok(())
}

#[tokio::test]
async fn rust_project_members_retain_added_order_through_reload_and_edit() -> Result<()> {
    let mut f = Projects::start().await?;
    let ids = [
        "ffffffff-ffff-4fff-8fff-ffffffffffff".to_owned(),
        "11111111-1111-4111-8111-111111111111".to_owned(),
        f.agent.clone(),
    ];
    for id in &ids[..2] {
        f.request(
            "/api/agents",
            Some(
                json!({"runtime": "api", "id": id, "name": id, "model": "fixture",
                "instructions": "Help"}),
            ),
            201,
        )
        .await?;
    }
    let members: Vec<Value> = ids
        .iter()
        .map(|agent| json!({"agent": agent, "write": false}))
        .collect();
    let project = f
        .request(
            "/api/projects",
            Some(
                json!({"id": new_id(), "name": "Ordered members", "members": members,
                "folders": []}),
            ),
            200,
        )
        .await?;
    let path = format!("/api/projects/{}", project_id(&project)?);
    let config = f.config.clone();
    f.restart(config).await?;
    ensure!(
        f.request(&path, None, 200).await?["members"] == json!(members),
        "members after reload"
    );
    let reordered: Vec<Value> = members.iter().rev().cloned().collect();
    f.request(
        &format!("{path}/edit"),
        Some(json!({"id": new_id(), "revision": project["revision"], "members": reordered})),
        200,
    )
    .await?;
    ensure!(
        f.request(&path, None, 200).await?["members"] == json!(reordered),
        "members after edit"
    );
    Ok(())
}
