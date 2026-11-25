//! Skill folders: validation, revisions and pinned file tools (port of
//! `app/skill-files.test.ts`).

use crate::sb_support::{after_tool, id, last_result, settle};
use minarbot_testkit::{
    control::{Answer, ControlModel},
    prelude::*,
    Server,
};
use std::sync::{Arc, Mutex};

fn files() -> Value {
    json!([
        {"path": "SKILL.md", "content": "\u{feff}---\r\nname: example-skill\r\ndescription: >-\r\n  Use for\r\n  examples.\r\nlicense: Apache-2.0\r\nmetadata:\r\n  version: \"1.2.3\"\r\n---\r\nRead references/detail.md before answering.\r\n"},
        {"path": "references/detail.md", "content": format!("Original details.\n{}", "x".repeat(9000))},
        {"path": "scripts/check.py", "content": "raise SystemExit(\"never execute\")\n"},
        {"path": "assets/sample.bin", "content": "AP+AAQ==", "encoding": "base64"},
    ])
}

fn with(extra: Value) -> Value {
    let mut files = files();
    if let Some(rows) = files.as_array_mut() {
        rows.push(extra);
    }
    files
}

/// The Node case called the TypeScript parser (`app/shared/skill-files.ts`).
/// The Rust server validates the same folder in `crates/shared/skill_files.rs`
/// behind `POST /api/skills`, so each input is checked there; a rejected
/// folder stores nothing. The export-bundle and draft-restore checks tested
/// browser modules (`exportedSkillFiles`, `restoreSkillCommand`) that the
/// Leptos importer (`crates/web/src/skill_import.rs`) replaced.
#[tokio::test]
async fn skill_folders_preserve_exact_files_and_reject_invalid_or_unsupported_inputs_as_a_whole(
) -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let reject = |files: Value| {
        let app = &app;
        async move {
            app.post_expect(
                "/api/skills",
                json!({"id": new_id(), "revision": 0, "files": files}),
                400,
            )
            .await
            .map_err(|error| format!("{error}: {files}"))
        }
    };
    for path in [
        "/etc/passwd",
        "../secret",
        "a/../b",
        "a\\b",
        "a//b",
        "a:",
        "",
        "SKILL.md",
        "skill.md",
    ] {
        reject(with(json!({"path": path, "content": "secret"}))).await?;
    }
    for content in [
        "bad",
        "---\nname: x\nname: y\ndescription: hi\n---\nbody",
        "---\nname: &a x\ndescription: *a\n---\nbody",
        "---\nname: x\ndescription: hi\nallowed-tools: Bash\n---\nbody",
        "---\nname: x\ndescription: hi\ndisable-model-invocation: true\n---\nbody",
        "---\nname: Invalid name\ndescription: hi\n---\nbody",
    ] {
        reject(json!([{"path": "SKILL.md", "content": content}])).await?;
    }
    for content in ["\u{0}binary".to_owned(), "x".repeat(48000)] {
        reject(with(json!({"path": "bad", "content": content}))).await?;
    }
    // JSON cannot carry the lone surrogate '\ud800' as UTF-8; send it escaped.
    let lone = r#"{"id":"ID","revision":0,"files":[{"path":"SKILL.md","content":"---\nname: test\ndescription: Test\n---\nBody"},{"path":"bad","content":"\ud800"}]}"#
        .replace("ID", &new_id());
    let response = app
        .request(minarbot_testkit::http::Method::POST, "/api/skills")?
        .header("Content-Type", "application/json")
        .body(lone)
        .send()
        .await?;
    ensure!(
        response.status().as_u16() == 400,
        "lone surrogate: {}",
        response.status()
    );
    for metadata in [
        "license: [MIT]".to_owned(),
        "compatibility: {os: linux}".to_owned(),
        "metadata: [one, two]".to_owned(),
        "metadata: {version: 123}".to_owned(),
        "compatibility: \"\"".to_owned(),
        format!("compatibility: {}", "x".repeat(501)),
    ] {
        reject(json!([{"path": "SKILL.md",
            "content": format!("---\nname: test\ndescription: Test\n{metadata}\n---\nBody")}]))
        .await?;
    }
    let many: Vec<Value> = (0..33)
        .map(|index| json!({"path": format!("f{index}"), "content": ""}))
        .collect();
    reject(json!(many)).await?;
    ensure!(
        app.get("/api/skills").await?["skills"] == json!([]),
        "a rejected folder was stored"
    );
    let saved = app
        .post(
            "/api/skills",
            json!({"id": new_id(), "revision": 0, "files": files()}),
        )
        .await?;
    ensure!(
        saved["description"] == "Use for examples.",
        "description: {saved}"
    );
    let stored = app.get(&format!("/api/skills/{}", id(&saved)?)).await?;
    ensure!(
        stored["files"] == files(),
        "stored files: {}",
        stored["files"]
    );
    Ok(())
}

#[derive(Default)]
struct Script {
    target: String,
    offset: u64,
    path: String,
    manifest: bool,
    results: Vec<Value>,
    offered: Vec<bool>,
}

async fn run(app: &Server, script: &Arc<Mutex<Script>>, path: &str) -> Result<Vec<Value>> {
    {
        let mut script = script.lock().map_err(|_| "script")?;
        script.results.clear();
    }
    let id = new_id();
    app.post_expect(path, json!({"id": id, "text": "Read skill files"}), 202)
        .await?;
    let row = settle(app, path, &id).await?;
    ensure!(row["status"] == "done", "{path}: {row}");
    let script = script.lock().map_err(|_| "script")?;
    Ok(script.results.clone())
}

#[tokio::test]
async fn folder_revisions_receipts_and_pinned_file_tools_survive_reload_without_foreign_or_revoked_access(
) -> Result<()> {
    let script = Arc::new(Mutex::new(Script {
        path: "references/detail.md".into(),
        ..Script::default()
    }));
    let model = ControlModel::start().await?;
    let shared = script.clone();
    model.respond(move |request| {
        let Ok(mut script) = shared.lock() else {
            return Answer::Status(500, json!({"error": "script"}));
        };
        script.offered.push(
            request
                .tool_names()
                .iter()
                .any(|name| name == "read_skill_file"),
        );
        if after_tool(request) {
            script.results.push(last_result(request));
            return Answer::text("Done");
        }
        if script.manifest {
            Answer::tool("read_skill", json!({"id": script.target}))
        } else {
            Answer::tool(
                "read_skill_file",
                json!({"id": script.target, "path": script.path, "offset": script.offset}),
            )
        }
    });
    let root = TestRoot::new()?;
    let mut app = root
        .start(ServerConfig::new().gateway(model.gateway()))
        .await?;
    let a = id(&app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "A", "model": "fixture", "instructions": "Help"}),
            201,
        )
        .await?)?;
    let b = id(&app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "B", "model": "fixture", "instructions": "Help"}),
            201,
        )
        .await?)?;
    let files = files();
    let command = json!({"id": new_id(), "revision": 0, "files": files});
    let mut invalid = command.clone();
    invalid["files"] = json!([{"path": "SKILL.md",
        "content": "---\nname: test\ndescription: Test\nmetadata: {version: 123}\n---\nBody"}]);
    app.post_expect("/api/skills", invalid, 400).await?;
    ensure!(
        app.get("/api/skills").await?["skills"] == json!([]),
        "invalid folder stored"
    );
    let skill = app.post("/api/skills", command.clone()).await?;
    let target = id(&skill)?;
    script.lock().map_err(|_| "script")?.target = target.clone();
    ensure!(
        app.post("/api/skills", command.clone()).await? == skill,
        "create replay differs"
    );
    let mut partial = command.clone();
    partial["files"] = json!([files[0]]);
    app.post_expect("/api/skills", partial, 409).await?;
    ensure!(
        app.get(&format!("/api/skills/{target}")).await?["files"] == files,
        "files changed"
    );
    let binding = format!("/api/agents/{a}/skills/{target}");
    let turns = format!("/api/agents/{a}/turns");
    app.post(
        &binding,
        json!({"id": new_id(), "revision": 0, "version": 1, "enabled": true}),
    )
    .await?;
    let mut changed = files.clone();
    changed[1]["content"] = json!("New details");
    let edit = json!({"id": new_id(), "revision": 1, "files": changed});
    app.post(&format!("/api/skills/{target}"), edit.clone())
        .await?;
    let mut other = edit.clone();
    other["id"] = json!(new_id());
    app.post_expect(&format!("/api/skills/{target}"), other, 409)
        .await?;
    app.post_expect(
        &format!("/api/skills/{target}"),
        json!({"id": new_id(), "revision": 2, "name": "Lossy", "description": "Would drop files", "content": "Body"}),
        409,
    )
    .await?;
    let detail = files[1]["content"].as_str().ok_or("detail")?.to_owned();
    let results = run(&app, &script, &turns).await?;
    ensure!(
        results[0]["content"] == detail[..8000],
        "first page: {}",
        results[0]
    );
    ensure!(results[0]["version"] == 1, "pinned version: {}", results[0]);
    ensure!(
        results[0]["next_offset"] == 8000,
        "next offset: {}",
        results[0]
    );
    script.lock().map_err(|_| "script")?.offset = 8000;
    let results = run(&app, &script, &turns).await?;
    ensure!(
        results[0]["content"] == detail[8000..],
        "second page: {}",
        results[0]
    );
    ensure!(
        results[0]["next_offset"].is_null(),
        "last page: {}",
        results[0]
    );
    {
        let mut script = script.lock().map_err(|_| "script")?;
        script.offset = 0;
        script.manifest = true;
    }
    let results = run(&app, &script, &turns).await?;
    let paths: Vec<&Value> = results[0]["files"]
        .as_array()
        .ok_or_else(|| format!("manifest: {}", results[0]))?
        .iter()
        .map(|file| &file["path"])
        .collect();
    let expected: Vec<&Value> = files
        .as_array()
        .ok_or("files")?
        .iter()
        .map(|file| &file["path"])
        .collect();
    ensure!(paths == expected, "manifest paths: {paths:?}");
    {
        let mut script = script.lock().map_err(|_| "script")?;
        script.manifest = false;
        script.path = "assets/sample.bin".into();
    }
    let results = run(&app, &script, &turns).await?;
    ensure!(
        results[0]["binary"] == true && results[0].get("content").is_none(),
        "binary file: {}",
        results[0]
    );
    for path in ["../secret", "/etc/passwd", "absent.txt"] {
        script.lock().map_err(|_| "script")?.path = path.into();
        let results = run(&app, &script, &turns).await?;
        ensure!(
            results[0]["error"]
                .as_str()
                .is_some_and(|error| error.contains("not found")),
            "{path}: {}",
            results[0]
        );
    }
    script.lock().map_err(|_| "script")?.path = "references/detail.md".into();
    let results = run(&app, &script, &format!("/api/agents/{b}/turns")).await?;
    ensure!(
        results[0]["error"]
            .as_str()
            .is_some_and(|error| error.contains("not found")),
        "foreign agent read a skill: {}",
        results[0]
    );
    app.restart().await?;
    let mut replayed = skill.clone();
    replayed["revision"] = json!(2);
    ensure!(
        app.post(&format!("/api/skills/{target}"), edit.clone())
            .await?
            == replayed,
        "edit receipt after restart"
    );
    ensure!(
        app.get(&format!("/api/skills/{target}?version=1")).await?["files"] == files,
        "version 1"
    );
    ensure!(
        app.get(&format!("/api/skills/{target}?version=2")).await?["files"] == changed,
        "version 2"
    );
    let results = run(&app, &script, &turns).await?;
    ensure!(
        results[0]["content"] == detail[..8000],
        "pinned after restart: {}",
        results[0]
    );
    app.post(
        &binding,
        json!({"id": new_id(), "revision": 1, "version": 2, "enabled": true}),
    )
    .await?;
    let results = run(&app, &script, &turns).await?;
    ensure!(
        results[0]["content"] == "New details",
        "version 2 read: {}",
        results[0]
    );
    app.post(
        &binding,
        json!({"id": new_id(), "revision": 2, "version": 2, "enabled": false}),
    )
    .await?;
    let results = run(&app, &script, &turns).await?;
    ensure!(
        results[0]["error"]
            .as_str()
            .is_some_and(|error| error.contains("not found")),
        "revoked skill read: {}",
        results[0]
    );
    let channel = id(&app
        .post_expect(
            "/api/channels",
            json!({"id": new_id(), "name": "Room", "instructions": "Help", "lead": a, "members": [a]}),
            201,
        )
        .await?)?;
    // Harness turns get a tool that is not offered as an MCP error result and
    // finish; the Node direct client failed the turn instead.
    script.lock().map_err(|_| "script")?.offered.clear();
    let results = run(&app, &script, &format!("/api/channels/{channel}/turns")).await?;
    let offered = script.lock().map_err(|_| "script")?.offered.clone();
    ensure!(
        offered.iter().all(|offered| !offered),
        "read_skill_file offered in a channel: {offered:?}"
    );
    ensure!(
        results.iter().all(|result| result.get("content").is_none()),
        "channel read skill content: {results:?}"
    );
    Ok(())
}
