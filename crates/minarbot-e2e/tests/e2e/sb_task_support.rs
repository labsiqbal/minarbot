//! Helpers for the `sb_task_*` ports of `app/task-recovery.test.ts`
//! (`canonicalTaskTools`, `preparedTask`, `routineTaskTools`).

use minarbot_testkit::{
    canonical::Account,
    new_id,
    protocol::{digest_command, sha256_hex},
    task_bridge::{lock_proof, random_token, tool_digest, Envelope, TaskFixture},
    Result,
};
use serde_json::{json, Value};

/// First row of a query (or `Null`).
pub fn one(f: &TaskFixture, sql: &str, params: impl minarbot_testkit::db::Params) -> Result<Value> {
    Ok(f.db()
        .rows(sql, params)?
        .into_iter()
        .next()
        .unwrap_or(Value::Null))
}

/// One column of the first row (or `Null`).
pub fn field(
    f: &TaskFixture,
    sql: &str,
    params: impl minarbot_testkit::db::Params,
    column: &str,
) -> Result<Value> {
    Ok(one(f, sql, params)?[column].clone())
}

/// A task acquired by the home device (`canonicalTaskTools().start`).
pub struct Task {
    pub id: String,
    pub token: String,
    pub ownership: Value,
    pub actor: Account,
    pub sequence: i64,
}

pub fn expected_revisions(payload: &Value) -> Value {
    let mut revisions = json!({"project": 1, "agent": 1, "homeBinding": 1, "membership": 1, "assignment": 1, "capability": 1});
    if payload["kind"] == "channel" {
        revisions["channel"] = json!(1);
    }
    if payload["kind"] == "side" {
        revisions["thread"] = json!(1);
    }
    revisions
}

/// Admit `payload` as `actor` (default: a direct "Ask first") and acquire it.
pub async fn start(f: &TaskFixture, actor: &Account, payload: Option<Value>) -> Result<Task> {
    let payload = payload.unwrap_or_else(
        || json!({"v": 1, "kind": "direct", "message": {"text": "Ask first", "files": []}}),
    );
    let command =
        f.command(json!({"payload": payload, "expectedRevisions": expected_revisions(&payload)}));
    f.admit(actor, &command).await?.expect_ok()?;
    let id = command["requestId"].as_str().ok_or("task id")?.to_owned();
    let (token, ownership, _) = f.acquire_task(&id).await?;
    Ok(Task {
        id,
        token,
        ownership,
        actor: actor.clone(),
        sequence: 0,
    })
}

/// A prepared and committed canonical tool action
/// (`canonicalTaskTools().stage`); `run` sends its context mutation.
pub struct Staged {
    pub request: Value,
    pub task: String,
    pub token: String,
}

impl Staged {
    pub async fn run(&self, f: &TaskFixture) -> Result<Envelope> {
        f.task_op(
            "context-mutations",
            &self.task,
            self.request.clone(),
            &self.token,
        )
        .await
    }

    pub fn action(&self) -> String {
        self.request["actionId"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }
}

pub async fn task_revision(f: &TaskFixture, task: &Task) -> Result<Value> {
    Ok(f.lookup(&task.actor, &task.id).await?.expect_ok()?.data()["revision"].clone())
}

pub async fn stage(f: &TaskFixture, task: &mut Task, args: &Value, name: &str) -> Result<Staged> {
    stage_raw(f, task, &args.to_string(), name).await
}

/// `stage` with the arguments string sent verbatim.
pub async fn stage_raw(
    f: &TaskFixture,
    task: &mut Task,
    arguments: &str,
    name: &str,
) -> Result<Staged> {
    let expected = task_revision(f, task).await?;
    let context = f
        .task_op(
            "context",
            &task.id,
            json!({"requestId": new_id(), "ownership": task.ownership, "expectedTaskRevision": expected}),
            &task.token,
        )
        .await?
        .expect_ok()?;
    let reference = context.data()["snapshotRef"].clone();
    let action = new_id();
    let sequence = task.sequence;
    task.sequence += 1;
    let tool_call = json!({"id": action, "function": {"name": name, "arguments": arguments}});
    let expected_revision = json!(expected.as_i64().unwrap_or(0) + 1);
    let payload_digest = tool_digest(&tool_call, &expected_revision, None);
    let intent = digest_command(&json!([
        task.id,
        action,
        sequence,
        task.ownership,
        payload_digest,
        "canonical",
        reference["snapshotId"],
        reference["digest"],
        null,
        null,
        1,
        null,
        null,
        null
    ]));
    f.prepare(
        &task.id,
        &action,
        json!({"requestId": new_id(), "sequence": sequence, "ownership": task.ownership, "payloadDigest": payload_digest,
            "actionKind": "canonical", "contextSnapshotId": reference["snapshotId"], "contextDigest": reference["digest"],
            "grantRevision": 1, "expectedTaskRevision": expected, "journalSequence": sequence + 1, "intentDigest": intent}),
        &task.token,
    )
    .await?
    .expect_ok()?;
    f.commit(
        &task.id,
        &action,
        json!({"requestId": new_id(), "ownership": task.ownership, "intentDigest": intent,
            "expectedTaskRevision": expected, "journalSequence": sequence + 1}),
        &task.token,
    )
    .await?
    .expect_ok()?;
    Ok(Staged {
        request: json!({"requestId": new_id(), "actionId": action, "ownership": task.ownership, "toolCall": tool_call,
            "payloadDigest": payload_digest, "expectedRevision": expected_revision}),
        task: task.id.clone(),
        token: task.token.clone(),
    })
}

/// `routineTaskTools`: a member-started task that is a routine occurrence step.
pub async fn routine_task(f: &TaskFixture) -> Result<(Task, String)> {
    f.db().execute(
        "INSERT INTO collaborator_agents VALUES (?,?,?,1,1)",
        [&f.project, &f.member.account_id, &f.agent],
    )?;
    let task = start(f, &f.member, None).await?;
    let routine = new_id();
    let occurrence = new_id();
    let db = f.db();
    db.execute_batch("BEGIN IMMEDIATE")?;
    db.execute(
        "INSERT INTO routines(id,agent,name,prompt,next_at,enabled) VALUES (?,?,'Outcome','Check work',1000,0)",
        [&routine, &f.agent],
    )?;
    db.execute(
        "INSERT INTO record_scopes VALUES ('routines',?,?,?,'project','agent',?)",
        [
            json!([routine]).to_string(),
            f.owner.account_id.clone(),
            f.project.clone(),
            f.agent.clone(),
        ],
    )?;
    db.execute(
        "INSERT INTO routine_occurrences(id,routine,project,due_at,schedule_revision,initiator_account,assignment_revision,authority_generation,home_device,zone,zone_revision,missed_count,state,graph_json,task)
         VALUES (?,?,?,1000,1,?,1,?,?,'Asia/Jakarta',1,0,'admitted',?,?)",
        [
            occurrence.clone(),
            routine.clone(),
            f.project.clone(),
            f.member.account_id.clone(),
            f.principal.authority_generation.clone(),
            f.home.clone(),
            json!({"root": f.agent, "nodes": [f.agent], "links": []}).to_string(),
            task.id.clone(),
        ],
    )?;
    db.execute(
        "INSERT INTO record_scopes VALUES ('routine_occurrences',?,?,?,'project','agent',?)",
        [
            json!([occurrence]).to_string(),
            f.owner.account_id.clone(),
            f.project.clone(),
            f.agent.clone(),
        ],
    )?;
    db.execute(
        "INSERT INTO routine_steps VALUES (?,?,?,'queued','')",
        [&occurrence, &f.agent, &task.id],
    )?;
    db.execute_batch("COMMIT")?;
    Ok((task, occurrence))
}

/// `preparedTask`: an acquired task with one prepared `read` action.
pub struct Prepared {
    pub task: String,
    pub token: String,
    pub ownership: Value,
    pub action: String,
    pub payload_digest: String,
    pub lock_proof: Value,
    pub commit: Value,
}

pub async fn prepared(f: &TaskFixture) -> Result<Prepared> {
    let command = f.command(json!({}));
    f.admit(&f.owner, &command).await?.expect_ok()?;
    let task = command["requestId"].as_str().ok_or("task")?.to_owned();
    let token = random_token()?;
    let proof = lock_proof();
    let acquired = f
        .acquire(
            json!({"requestId": new_id(), "taskId": task, "previousExecutorGeneration": 0,
                "executorTokenHash": sha256_hex(&token), "lockProof": proof}),
            &token,
        )
        .await?
        .expect_ok()?;
    let ownership = acquired.data()["ownership"].clone();
    let revision = acquired.receipt()["recordRevision"].clone();
    let context = f
        .task_op("context", &task, json!({"requestId": new_id(), "ownership": ownership, "expectedTaskRevision": revision}), &token)
        .await?
        .expect_ok()?;
    let reference = context.data()["snapshotRef"].clone();
    let action = new_id();
    let payload_digest = sha256_hex("read");
    let intent = digest_command(&json!([
        task,
        action,
        0,
        ownership,
        payload_digest,
        "read",
        reference["snapshotId"],
        reference["digest"],
        null,
        null,
        1,
        null,
        null,
        null
    ]));
    f.prepare(
        &task,
        &action,
        json!({"requestId": new_id(), "sequence": 0, "ownership": ownership, "payloadDigest": payload_digest,
            "actionKind": "read", "contextSnapshotId": reference["snapshotId"], "contextDigest": reference["digest"],
            "grantRevision": 1, "expectedTaskRevision": revision, "journalSequence": 1, "intentDigest": intent}),
        &token,
    )
    .await?
    .expect_ok()?;
    let commit = json!({"requestId": new_id(), "ownership": ownership, "intentDigest": intent,
        "expectedTaskRevision": revision, "journalSequence": 1});
    Ok(Prepared {
        task,
        token,
        ownership,
        action,
        payload_digest,
        lock_proof: proof,
        commit,
    })
}

/// Canonical workspace backup of the fixture database into `file` (the Node
/// `workspaceSnapshot(db, owner)` for a canonical schema).
pub fn canonical_snapshot(f: &TaskFixture, file: &std::path::Path, owner: &str) -> Result<()> {
    f.db().execute("VACUUM INTO ?", [file.to_string_lossy()])?;
    let snapshot = minarbot_testkit::db::Fixture::open(file)?;
    snapshot.execute_batch(
        "CREATE TABLE IF NOT EXISTS minarbot_backup (format TEXT NOT NULL,version INTEGER NOT NULL,created TEXT NOT NULL) STRICT;
         DELETE FROM minarbot_backup;
         INSERT INTO minarbot_backup VALUES ('minarbot-canonical-v6',6,'2026-11-18T11:06:40.000Z');
         CREATE TABLE IF NOT EXISTS minarbot_backup_authority(generation TEXT NOT NULL,owner_account TEXT REFERENCES accounts(id)) STRICT;
         DELETE FROM minarbot_backup_authority;",
    )?;
    let generation: String =
        snapshot.scalar("SELECT generation FROM authority_state WHERE id=1", [])?;
    snapshot.execute(
        "INSERT INTO minarbot_backup_authority VALUES (?,?)",
        [generation, owner.to_owned()],
    )?;
    Ok(())
}

/// `storedTurnProvision`: the provision inside a `wait_provisions.request_json`,
/// with its payload digest checked.
pub fn stored_provision(raw: &Value) -> Result<Value> {
    let value: Value = serde_json::from_str(raw.as_str().ok_or("request_json")?)?;
    let provision = value["provision"].clone();
    let mut payload = provision.clone();
    if let Some(fields) = payload.as_object_mut() {
        fields.remove("payloadDigest");
    }
    let digest = digest_command(&json!(["minarbot-wait-provision-v1", payload]));
    if provision["payloadDigest"] != digest.as_str() {
        return Err(format!("provision digest mismatch: {provision}").into());
    }
    Ok(provision)
}

/// A ready wait secret result for `provision`.
pub fn ready_result(provision: &Value) -> Value {
    json!({"requestId": new_id(), "provisionId": provision["provisionId"], "payloadDigest": provision["payloadDigest"],
        "waitKey": provision["waitKey"], "secretGeneration": 1, "sourceDeviceGeneration": 1, "state": "ready",
        "secretRef": json!([provision["waitKey"], 1]).to_string()})
}
