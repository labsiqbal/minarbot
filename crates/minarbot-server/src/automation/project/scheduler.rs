use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use sha2::{Digest, Sha256};

fn identity(v: &Value) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(crate::db::canonical(v).as_bytes()))
}
fn scan(db: &Connection, kind: &str, sql: &str) -> Result<Vec<Value>> {
    let cursor = one(
        db,
        "SELECT after_id FROM routine_scheduler_cursors WHERE kind=?",
        [kind],
    )?
    .ok_or_else(Failure::storage)?;
    let rows = all(db, sql, [string(&cursor, "after_id")])?;
    let after = rows
        .last()
        .filter(|_| rows.len() == 100)
        .map(|r| string(r, "id"))
        .unwrap_or("");
    if after != string(&cursor, "after_id") {
        db.execute(
            "UPDATE routine_scheduler_cursors SET after_id=? WHERE kind=?",
            params![after, kind],
        )?;
    }
    Ok(rows)
}
fn generation(db: &Connection) -> Result<Value> {
    Ok(one(
        db,
        "SELECT generation FROM authority_state WHERE id=1 AND serving_state='active'",
        [],
    )?
    .ok_or_else(Failure::storage)?["generation"]
        .clone())
}
fn overdue(
    s: &Scope,
    spec: &Value,
    due: i64,
    zone: &str,
    routine: bool,
    pending: Option<&Value>,
) -> Result<(i64, Option<i64>, i64)> {
    let duration = schedule::interval(spec);
    if spec["kind"] != "cron" {
        let missed = (if duration > 0 {
            (s.state.config.now() - due) / duration
        } else {
            0
        })
        .max(if routine {
            pending
                .and_then(|p| p["missed_count"].as_i64())
                .unwrap_or(0)
        } else {
            0
        });
        return Ok((
            missed,
            if duration > 0 {
                due.checked_add((missed + 1).saturating_mul(duration))
                    .filter(|n| *n <= schedule::MAX_TIME)
            } else {
                None
            },
            s.state.config.now(),
        ));
    }
    let cursor = if let Some(pending) = pending {
        one(
            s.db,
            &format!(
                "SELECT counted_at FROM {} WHERE occurrence_id=?",
                if routine {
                    "routine_cron_cursors"
                } else {
                    "workflow_schedule_cron_cursors"
                }
            ),
            [string(
                pending,
                if routine { "id" } else { "occurrence_id" },
            )],
        )?
    } else {
        None
    };
    let counted = s.state.config.now().max(
        cursor
            .as_ref()
            .and_then(|c| c["counted_at"].as_i64())
            .unwrap_or(s.state.config.now()),
    );
    let count = if cursor.is_some() {
        pending
            .and_then(|p| p["missed_count"].as_i64())
            .unwrap_or(0)
    } else {
        -1
    };
    let (missed, next) = schedule::advance_cron(
        string(spec, "expression"),
        zone,
        cursor
            .as_ref()
            .and_then(|c| c["counted_at"].as_i64())
            .unwrap_or(due - 1),
        counted,
        routine,
    )?;
    Ok(((count + missed).max(0), next, counted))
}
fn wall_identity(at: i64, zone: &str) -> Result<(Value, u8)> {
    use chrono::{LocalResult, TimeZone};
    let zone = zone
        .parse::<chrono_tz::Tz>()
        .map_err(|_| Failure::storage())?;
    let local = chrono::DateTime::from_timestamp_millis(at)
        .ok_or_else(Failure::storage)?
        .with_timezone(&zone);
    let index = match zone.from_local_datetime(&local.naive_local()) {
        LocalResult::Ambiguous(a, b) => u8::from(local == a.max(b)),
        _ => 0,
    };
    Ok((json!(local.format("%Y-%m-%dT%H:%M").to_string()), index))
}
fn ready(device: &Value, now: i64) -> bool {
    device["status"] == "ready"
        && device["last_seen"]
            .as_i64()
            .is_some_and(|at| at >= now - 30000)
}
fn routine_schedule(s: &Scope, binding: &Value) -> Result<()> {
    let r = routines::root(s, string(binding, "routine"))?;
    let (home, device) = s.device(string(&r, "agent"))?;
    if binding["authority_generation"] != generation(s.db)?
        || binding["assignment_revision"] != home["assignment"]
        || binding["home_device"] != device["id"]
    {
        return Ok(());
    }
    let spec = routines::spec(s, &r)?;
    let mut due = r["next_at"].as_i64().ok_or_else(Failure::storage)?;
    let pending=one(s.db,"SELECT * FROM routine_occurrences WHERE routine=? AND schedule_revision=? AND source='schedule' AND state='pending'",params![r["id"].as_str(),r["revision"].as_i64()])?;
    let changed =
        binding["zone"] != device["zone"] || binding["zone_revision"] != device["zone_revision"];
    if changed {
        let consumed=exists(s.db,"SELECT 1 FROM routine_occurrences WHERE routine=? AND source='schedule' AND due_at=? AND task IS NOT NULL",params![r["id"].as_str(),due])?;
        if pending.is_none() && !consumed && ["once", "cron"].contains(&string(&spec, "kind")) {
            due = routines::timing(&spec, string(&device, "zone"), s.state.config.now())?;
        }
        s.db.execute(
            "UPDATE routine_schedule_bindings SET zone=?,zone_revision=? WHERE routine=?",
            params![
                device["zone"].as_str(),
                device["zone_revision"].as_i64(),
                r["id"].as_str()
            ],
        )?;
        s.db.execute(
            "UPDATE routines SET next_at=? WHERE id=?",
            params![due, r["id"].as_str()],
        )?;
        s.observe("routine", string(&r, "id"))?;
    }
    if r["enabled"] != 1 || r["trigger"] != "schedule" || due > s.state.config.now() {
        return Ok(());
    }
    let (missed, next, counted) = overdue(
        s,
        &spec,
        due,
        if pending.is_some() {
            string(binding, "zone")
        } else {
            string(&device, "zone")
        },
        true,
        pending.as_ref(),
    )?;
    let occurrence = if let Some(p) = &pending {
        string(p, "id").to_owned()
    } else {
        identity(&json!([
            r["id"],
            r["revision"],
            device["zone_revision"],
            due,
            0
        ]))
    };
    if pending.is_none() {
        let mapping = s.scope("routines", json!([r["id"]]), string(&r, "agent"))?;
        s.db.execute("INSERT INTO routine_occurrences(id,routine,project,due_at,schedule_revision,initiator_account,assignment_revision,authority_generation,home_device,zone,zone_revision,missed_count,state,schedule_kind) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,'pending',?)",params![occurrence,r["id"].as_str(),s.project,due,r["revision"].as_i64(),s.account,home["assignment"].as_i64(),binding["authority_generation"].as_str(),device["id"].as_str(),device["zone"].as_str(),device["zone_revision"].as_i64(),missed,spec["kind"].as_str()])?;
        s.map(
            "routine_occurrences",
            json!([occurrence]),
            string(&r, "agent"),
            &json!({"account":s.account,"visibility":mapping["visibility"]}),
        )?;
        s.observe("routine_occurrence", &occurrence)?;
    } else if pending
        .as_ref()
        .is_some_and(|p| p["missed_count"] != missed)
    {
        s.db.execute(
            "UPDATE routine_occurrences SET missed_count=? WHERE id=?",
            params![missed, occurrence],
        )?;
        s.observe("routine_occurrence", &occurrence)?;
    }
    if spec["kind"] == "cron" {
        s.db.execute("INSERT INTO routine_cron_cursors VALUES(?,?,?) ON CONFLICT(routine) DO UPDATE SET occurrence_id=excluded.occurrence_id,counted_at=excluded.counted_at",params![r["id"].as_str(),occurrence,counted])?;
    }
    if !ready(&device, s.state.config.now())
        || exists(
            s.db,
            "SELECT 1 FROM routine_occurrences WHERE routine=? AND state='admitted'",
            [string(&r, "id")],
        )?
    {
        return Ok(());
    }
    let graph = routines::flow(s, &r)?["graph"].clone();
    for agent in graph["nodes"].as_array().into_iter().flatten() {
        s.device(agent.as_str().ok_or_else(Failure::storage)?)?;
    }
    runtime::admit_routine(s, &r, &occurrence, &graph, None)?;
    let next = if changed && spec["kind"] == "cron" {
        Some(routines::timing(
            &spec,
            string(&device, "zone"),
            s.state.config.now(),
        )?)
    } else {
        next
    };
    s.db.execute(
        "UPDATE routines SET next_at=?,enabled=?,revision=revision+1 WHERE id=?",
        params![next.unwrap_or(due), next.is_some(), r["id"].as_str()],
    )?;
    s.observe("routine", string(&r, "id"))?;
    s.observe("routine_occurrence", &occurrence)
}
fn settle(s: &Scope, occurrence: &Value) -> Result<()> {
    let r = routines::root(s, string(occurrence, "routine"))?;
    let id = string(occurrence, "id");
    let mapping = s.scope("routine_occurrences", json!([id]), string(&r, "agent"))?;
    if occurrence["authority_generation"] != generation(s.db)? {
        return Ok(());
    }
    for step in all(s.db,"SELECT s.agent,k.state,t.no_update,t.reply,t.error FROM routine_steps s JOIN tasks k ON k.logical_turn=s.turn JOIN turns t ON t.id=k.logical_turn WHERE s.occurrence=? AND s.state='queued' AND NOT EXISTS(SELECT 1 FROM tasks successor WHERE successor.predecessor_task=k.id)",[id])? {
        if !["completed","failed","cancelled"].contains(&string(&step,"state")){continue}
        s.db.execute("UPDATE routine_steps SET state=?,result=? WHERE occurrence=? AND agent=?",params![if step["state"]=="completed"{if step["no_update"]==1{"skipped"}else{"done"}}else{"failed"},step["reply"].as_str().or(step["error"].as_str()).unwrap_or(""),id,step["agent"].as_str()])?;
    }
    let graph = data::json(&occurrence["graph_json"])?;
    for _ in 0..32 {
        let rows = all(
            s.db,
            "SELECT agent,state,result FROM routine_steps WHERE occurrence=? ORDER BY agent",
            [id],
        )?;
        let mut changed = false;
        for step in rows.iter().filter(|r| r["state"] == "waiting") {
            let inputs = graph["links"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|l| l["to"] == step["agent"])
                .filter_map(|l| rows.iter().find(|r| r["agent"] == l["from"]))
                .collect::<Vec<_>>();
            if inputs
                .iter()
                .any(|r| !["done", "failed", "skipped"].contains(&string(r, "state")))
            {
                continue;
            }
            let inputs = inputs
                .iter()
                .map(|r| json!({"agent":r["agent"],"status":r["state"],"text":r["result"]}))
                .collect::<Vec<_>>();
            let prompt=format!("Continue this routine flow using the completed upstream results. Failed inputs are context, not successful work. Follow your own instructions and permissions.\n\n{}",crate::db::canonical(&json!(inputs)));
            if inputs.iter().all(|r| r["status"] != "done") || prompt.len() > 256 * 1024 {
                let skip = inputs.iter().all(|r| r["status"] != "done");
                s.db.execute(
                    "UPDATE routine_steps SET state=?,result=? WHERE occurrence=? AND agent=?",
                    params![
                        if skip { "skipped" } else { "failed" },
                        if skip {
                            "No successful input."
                        } else {
                            "Combined flow input exceeds 256 KiB."
                        },
                        id,
                        step["agent"].as_str()
                    ],
                )?;
            } else {
                let agent = string(step, "agent");
                let task = runtime::task(
                    s,
                    agent,
                    string(&r, "name"),
                    &prompt,
                    string(&mapping, "visibility"),
                    &format!("tasks:routine-step:{id}:{agent}"),
                )?;
                s.db.execute(
                    "UPDATE routine_steps SET turn=?,state='queued' WHERE occurrence=? AND agent=?",
                    params![task["taskId"].as_str(), id, agent],
                )?;
            }
            changed = true;
        }
        if !changed {
            break;
        }
    }
    if !exists(
        s.db,
        "SELECT 1 FROM routine_steps WHERE occurrence=? AND state IN('waiting','queued')",
        [id],
    )? {
        let failed = exists(
            s.db,
            "SELECT 1 FROM routine_steps WHERE occurrence=? AND state='failed'",
            [id],
        )?;
        s.db.execute(
            "UPDATE routine_occurrences SET state=? WHERE id=?",
            [if failed { "failed" } else { "done" }, id],
        )?;
        s.observe("routine_occurrence", id)?;
    }
    Ok(())
}
fn workflow_schedule(s: &Scope, row: &Value) -> Result<()> {
    let id = string(row, "id");
    let w = workflows::root(s, id)?;
    let (home, device) = s.device(string(&w, "agent"))?;
    if w["deleted"] != 0
        || row["authority_generation"] != generation(s.db)?
        || row["assignment_revision"] != home["assignment"]
        || row["home_device"] != device["id"]
    {
        return Ok(());
    }
    let spec = data::json(&row["spec_json"])?;
    let pending=one(s.db,"SELECT * FROM occurrences WHERE schedule=? AND schedule_revision=? AND state IN('pending','blocked')",params![id,row["revision"].as_i64()])?;
    let mut due = row["next_at"].as_i64();
    if row["zone"] != device["zone"] || row["zone_revision"] != device["zone_revision"] {
        if pending.is_none() && due.is_some() && ["once", "cron"].contains(&string(&spec, "kind")) {
            due = Some(if spec["kind"] == "cron" {
                schedule::next_cron(
                    string(&spec, "expression"),
                    string(&device, "zone"),
                    s.state.config.now(),
                    false,
                )
                .map_err(routines::schedule_error)?
            } else {
                routines::timing(&spec, string(&device, "zone"), s.state.config.now())?
            });
        }
        s.db.execute(
            "UPDATE schedules SET zone=?,zone_revision=?,next_at=? WHERE id=?",
            params![
                device["zone"].as_str(),
                device["zone_revision"].as_i64(),
                due,
                id
            ],
        )?;
        s.observe("workflow_schedule", id)?;
    }
    let Some(due) = due.filter(|at| *at <= s.state.config.now()) else {
        return Ok(());
    };
    let changed = row["zone"] != device["zone"] || row["zone_revision"] != device["zone_revision"];
    let (missed, mut next, counted) = overdue(
        s,
        &spec,
        due,
        if pending.is_some() {
            string(row, "zone")
        } else {
            string(&device, "zone")
        },
        false,
        pending.as_ref(),
    )?;
    if changed && spec["kind"] == "cron" {
        schedule::scan(
            string(&spec, "expression"),
            string(&device, "zone"),
            counted,
            counted.saturating_add(1825 * 86400000),
            |at, _| {
                next = Some(at);
                Ok(false)
            },
        )?;
    }
    let (local, index) = if ["once", "cron"].contains(&string(&spec, "kind")) {
        wall_identity(due, string(&device, "zone"))?
    } else {
        (json!(due), 0)
    };
    let occurrence = pending
        .as_ref()
        .map(|p| string(p, "occurrence_id").to_owned())
        .unwrap_or_else(|| {
            identity(&json!([
                id,
                row["revision"],
                device["zone_revision"],
                local,
                index
            ]))
        });
    let request = pending
        .as_ref()
        .map(|p| string(p, "admission_id").to_owned())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if pending.is_none() {
        s.db.execute("INSERT INTO occurrences(schedule,occurrence_id,due_at,schedule_revision,zone_revision,initiator_account,assignment_revision,authority_generation,task,state,missed_count,run,admission_id) VALUES(?,?,?,?,?,?,?,?,NULL,'pending',?,NULL,?)",params![id,occurrence,due,row["revision"].as_i64(),device["zone_revision"].as_i64(),s.account,home["assignment"].as_i64(),row["authority_generation"].as_str(),missed,request])?;
    } else if pending
        .as_ref()
        .is_some_and(|p| p["missed_count"] != missed)
    {
        s.db.execute(
            "UPDATE occurrences SET missed_count=? WHERE schedule=? AND occurrence_id=?",
            params![missed, id, occurrence],
        )?;
    }
    if pending.as_ref().is_none_or(|p| p["missed_count"] != missed) {
        s.observe(
            "workflow_schedule_occurrence",
            &json!([id, occurrence]).to_string(),
        )?;
    }
    if spec["kind"] == "cron" {
        s.db.execute("INSERT INTO workflow_schedule_cron_cursors VALUES(?,?,?) ON CONFLICT(schedule) DO UPDATE SET occurrence_id=excluded.occurrence_id,counted_at=excluded.counted_at",params![id,occurrence,counted])?;
    }
    if !ready(&device,s.state.config.now())||exists(s.db,"SELECT 1 FROM workflow_runs WHERE workflow=? AND status IN('ready','waiting','approval')",[id])?{return Ok(())}
    let g = workflows::graph(
        s,
        string(&w, "agent"),
        &workflows::version(s, id, w["revision"].as_i64().ok_or_else(Failure::storage)?)?,
    )?;
    let input = one(
        s.db,
        "SELECT input FROM workflow_schedule_targets WHERE schedule=?",
        [id],
    )?
    .ok_or_else(Failure::storage)?;
    let digest = crate::db::digest(&crate::db::canonical(&json!([
        "minarbot-workflow-occurrence-v1",
        id,
        occurrence,
        row["revision"],
        s.account,
        input["input"]
    ])));
    let count = one(
        s.db,
        "SELECT count(*) AS n FROM workflow_runs WHERE status IN('ready','waiting','approval')",
        [],
    )?
    .ok_or_else(Failure::storage)?;
    if count["n"].as_i64().unwrap_or(0) >= 100 {
        return Ok(());
    }
    s.db.execute("INSERT INTO workflow_runs(id,workflow,version,agent,graph,input,last,cursor,status,created) VALUES(?,?,?,?,?,?,?,?,'ready',?)",params![request,id,w["revision"].as_i64(),w["agent"].as_str(),crate::db::canonical(&g),input["input"].as_str(),input["input"].as_str(),g["entry"].as_str(),iso(s.db,s.state.config.now())?])?;
    let mapping = s.scope("workflows", json!([id]), id)?;
    s.map(
        "workflow_runs",
        json!([request]),
        id,
        &json!({"account":s.account,"visibility":mapping["visibility"]}),
    )?;
    s.save(
        &format!("/api/projects/{}/workflows/{id}/run", s.project),
        &request,
        &digest,
        201,
        &request,
        1,
        json!({"id":request,"projectId":s.project,"homeDeviceId":device["id"],"revision":1}),
    )?;
    s.db.execute("UPDATE occurrences SET state='admitted',run=admission_id,missed_count=? WHERE schedule=? AND occurrence_id=?",params![missed,id,occurrence])?;
    s.db.execute(
        "UPDATE schedules SET next_at=?,enabled=? WHERE id=?",
        params![next, next.is_some(), id],
    )?;
    s.observe("workflow_run", &request)?;
    s.observe("workflow_schedule", id)?;
    s.observe(
        "workflow_schedule_occurrence",
        &json!([id, occurrence]).to_string(),
    )
}
pub(super) fn maintain(state: &AppState, db: &Connection) -> Result<()> {
    if !exists(
        db,
        "SELECT 1 FROM authority_state WHERE id=1 AND serving_state='active'",
        [],
    )? {
        return Ok(());
    }
    for (kind,sql) in [
        ("flow","SELECT * FROM routine_occurrences WHERE state='admitted' AND id>? ORDER BY id LIMIT 100"),
        ("schedule","SELECT b.*,b.routine AS id FROM routine_schedule_bindings b WHERE b.routine>? ORDER BY b.routine LIMIT 100"),
        ("workflow_schedule","SELECT * FROM schedules WHERE enabled=1 AND id>? ORDER BY id LIMIT 100"),
    ] {
        for row in maintenance_transaction(db,||scan(db,kind,sql))? {
            let result=maintenance_transaction(db,||{
                let s=Scope{state,db,account:string(&row,"initiator_account"),project:string(&row,"project"),kind:if kind=="workflow_schedule"{"Workflow"}else{"Routine"}};
                match kind {"flow"=>settle(&s,&row),"schedule"=>routine_schedule(&s,&row),_=>workflow_schedule(&s,&row)}
            });
            if let Err(e)=result{if e.status>=500{return Err(e)}}
        }
    }
    Ok(())
}
