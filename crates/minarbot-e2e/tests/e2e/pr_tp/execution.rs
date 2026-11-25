//! Home execution loop: provider answers, setup tools, uncertainty and
//! completion recovery through signed canonical HTTP.

use super::{
    fixture::{digest, direct, fresh_revisions, Tp},
    routines::text,
};
use minarbot_testkit::{device_host::DeviceHost, prelude::*};

pub struct LoopTask {
    pub task: String,
    pub agent: String,
    pub project: String,
    pub binding: Option<String>,
}

/// Binding to create for a loop task (`loopTask`'s `existingBinding`).
pub enum Binding {
    New,
    None,
    Existing(String),
}

/// `loopTask`: a project, an agent on `runtime`'s home and one admitted
/// direct task (or a setup agent whose task is the agent id).
pub async fn loop_task(tp: &Tp, runtime: &DeviceHost, setup: bool, binding: Binding) -> Result<LoopTask> {
    let device = runtime.device_id()?;
    let home = tp
        .data("/api/devices")
        .await?
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == device.as_str()).cloned())
        .ok_or("loop home missing")?;
    let (project, agent) = (new_id(), new_id());
    tp.project(&project, "Execution loop").await?;
    let binding = match binding {
        Binding::New => {
            let binding = new_id();
            let seed = tp.write()?;
            let account: String = seed.scalar("SELECT account FROM devices WHERE id=?", [&device])?;
            seed.execute(
                "INSERT INTO provider_bindings VALUES (?,?,?,'openai-compatible',?,'Synthetic','ready',1)",
                [&binding, &device, &account, &binding],
            )?;
            Some(binding)
        }
        Binding::None => None,
        Binding::Existing(binding) => Some(binding),
    };
    let body = json!({"requestId": agent, "id": agent, "name": "Loop agent", "model": "fixture",
        "backupModel": "", "instructions": "Answer carefully", "threadLimit": 2,
        "projectId": project, "homeDeviceId": home["id"], "expectedProjectRevision": 1,
        "expectedHomeDeviceRevision": home["revision"],
        "providerBinding": binding.as_ref().map(|binding| json!({"bindingId": binding, "expectedRevision": 1})),
        "setup": setup});
    let (status, created) = tp.send("/api/agents", Some(body)).await?;
    ensure!(status == 201, "loop agent: {status} {created}");
    let task = if setup { agent.clone() } else { new_id() };
    if !setup {
        let payload = direct("Answer once");
        let revisions = fresh_revisions();
        let home_id = text(&home["id"]);
        let (status, admitted) = tp
            .send(
                "/api/tasks",
                Some(json!({"requestId": task, "projectId": project, "agentId": agent,
                    "homeDeviceId": home_id, "payload": payload, "expectedRevisions": revisions,
                    "payloadDigest": digest(&json!(["minarbot-task-payload-v1", task, project, agent,
                        home_id, payload, revisions]))})),
            )
            .await?;
        ensure!(status == 202, "loop task: {status} {admitted}");
    }
    Ok(LoopTask { task, agent, project, binding })
}
