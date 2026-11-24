use super::*;
use crate::turns::messaging;
const AGENT: &[&str] = &[
    "id",
    "name",
    "role",
    "model",
    "runtime",
    "instructions",
    "tool_budget",
    "revision",
    "status",
    "hidden",
];
fn deny() -> Failure {
    error(403, "MCP authorization failed.")
}
fn pick(v: &Value, keys: &[&str]) -> Value {
    Value::Object(
        keys.iter()
            .filter_map(|k| v.get(*k).map(|v| ((*k).into(), v.clone())))
            .collect(),
    )
}
fn own(db: &Connection, c: &Value, agent: &str) -> Result<()> {
    if c["kind"] == "session" && c["agent"] != agent {
        return Err(deny());
    }
    data::agents::read(db, agent)?;
    Ok(())
}
fn owner(c: &Value) -> Result<()> {
    if c["kind"] == "peer" {
        Ok(())
    } else {
        Err(deny())
    }
}
fn current(db: &Connection, c: &Value) -> Result<Value> {
    one(
        db,
        "SELECT * FROM turns WHERE id=? AND agent=? AND status='running'",
        [string(c, "turn"), string(c, "agent")],
    )?
    .ok_or_else(deny)
}
fn member(db: &Connection, c: &Value, id: &str) -> Result<Value> {
    let p = data::projects::read(db, id)?;
    if c["kind"] == "session"
        && (p["archived"] == true
            || !p["members"]
                .as_array()
                .is_some_and(|m| m.iter().any(|m| m["agent"] == c["agent"])))
    {
        return Err(deny());
    }
    Ok(p)
}
fn project_of(db: &Connection, t: &Value) -> Result<Option<Value>> {
    one(db,"SELECT project FROM local_project_threads WHERE thread=? UNION SELECT project FROM local_project_sections WHERE channel=?",[t["thread"].as_str(),t["channel"].as_str()])
}
fn conversation(db: &Connection, c: &Value, kind: &str, id: &str) -> Result<()> {
    if c["kind"] == "session" {
        let t = current(db, c)?;
        let same = match kind {
            "direct" => t["agent"] == id && t["thread"].is_null() && t["channel"].is_null(),
            "thread" => t["thread"] == id,
            "section" => t["channel"] == id,
            _ => false,
        };
        if !same {
            return Err(deny());
        }
    }
    match kind {
        "direct" => own(db, c, id)?,
        "thread" => {
            let t = one(db, "SELECT agent FROM threads WHERE id=?", [id])?
                .ok_or_else(|| error(404, "Conversation not found."))?;
            if c["kind"] == "session" {
                own(db, c, string(&t, "agent"))?;
            }
            if let Some(p) = one(
                db,
                "SELECT project FROM local_project_threads WHERE thread=?",
                [id],
            )? {
                member(db, c, string(&p, "project"))?;
            }
        }
        "section" => {
            let p = one(
                db,
                "SELECT project FROM local_project_sections WHERE channel=?",
                [id],
            )?
            .ok_or_else(|| error(404, "Section not found."))?;
            member(db, c, string(&p, "project"))?;
        }
        _ => return Err(error(400, "Invalid conversation kind.")),
    }
    Ok(())
}
fn routine(db: &Connection, c: &Value, id: &str, write: bool) -> Result<Value> {
    let r = one(db, "SELECT * FROM routines WHERE id=?", [id])?
        .ok_or_else(|| error(404, "Routine not found."))?;
    if let Some(p) = r["project"].as_str() {
        member(db, c, p)?;
    } else {
        own(db, c, string(&r, "agent"))?;
    }
    if write && c["kind"] == "session" {
        own(db, c, string(&r, "agent"))?;
        if let Some(flow) = one(db, "SELECT graph FROM routine_flows WHERE routine=?", [id])? {
            let g = data::json(&flow["graph"])?;
            for n in g["nodes"].as_array().into_iter().flatten() {
                let agent = n.as_str().unwrap_or_else(|| string(n, "agent"));
                if let Some(p) = r["project"].as_str() {
                    if !member(db, c, p)?["members"]
                        .as_array()
                        .is_some_and(|v| v.iter().any(|m| m["agent"] == agent))
                    {
                        return Err(deny());
                    }
                } else {
                    own(db, c, agent)?;
                }
            }
        }
    }
    Ok(r)
}
fn workflow(db: &Connection, c: &Value, id: &str, run: bool) -> Result<()> {
    let row=one(db,if run{"SELECT agent,graph FROM workflow_runs WHERE id=?"}else{"SELECT w.agent,v.graph FROM workflows w JOIN workflow_versions v ON v.workflow=w.id WHERE w.id=? ORDER BY v.revision DESC LIMIT 1"},[id])?.ok_or_else(||error(404,"Workflow not found."))?;
    own(db, c, string(&row, "agent"))?;
    if c["kind"] == "session" {
        let g = data::json(&row["graph"])?;
        for n in g["nodes"].as_array().into_iter().flatten() {
            if let Some(a) = n["agent"].as_str() {
                own(db, c, a)?;
            }
            if let Some(ch) = n["channel"].as_str() {
                let p = one(
                    db,
                    "SELECT project FROM local_project_sections WHERE channel=?",
                    [ch],
                )?
                .ok_or_else(deny)?;
                member(db, c, string(&p, "project"))?;
            }
        }
    }
    Ok(())
}
pub fn authorize(
    state: &AppState,
    db: &Connection,
    c: &Value,
    name: &str,
    a: &Value,
) -> Result<()> {
    let token = one(
        db,
        "SELECT revoked,expires FROM mcp_server_tokens WHERE id=?",
        [string(c, "id")],
    )?
    .ok_or_else(deny)?;
    if !token["revoked"].is_null()
        || token["expires"]
            .as_i64()
            .is_some_and(|v| v <= state.config.now())
    {
        return Err(deny());
    }
    if c["kind"] == "session" {
        if let Some(p) = project_of(db, &current(db, c)?)? {
            member(db, c, string(&p, "project"))?;
        }
    }
    match name {
        "web_fetch" | "web_search"
            if c["kind"] != "session"
                || !crate::turns::web::access(state, db, string(c, "agent"))?.offered(name) =>
        {
            return Err(deny());
        }
        "create_agent" | "create_project" => owner(c)?,
        "hide_agent" => {
            owner(c)?;
            own(db, c, string(a, "agent"))?;
        }
        "read_agent"
        | "update_agent"
        | "list_conversations"
        | "create_conversation"
        | "read_computer_status"
        | "open_computer" => own(db, c, string(a, "agent"))?,
        "read_conversation" => conversation(db, c, string(a, "kind"), string(a, "conversation"))?,
        "rename_conversation"
        | "archive_conversation"
        | "restore_conversation"
        | "delete_conversation" => {
            if name == "delete_conversation" {
                owner(c)?;
            }
            conversation(db, c, "thread", string(a, "thread"))?;
            if name == "archive_conversation" && c["kind"] == "session" {
                return Err(error(
                    409,
                    "A session cannot archive its own running conversation.",
                ));
            }
        }
        "read_project" | "add_project_section" | "list_project_environment" => {
            member(db, c, string(a, "project"))?;
        }
        "add_project_member" | "remove_project_member" => {
            owner(c)?;
            member(db, c, string(a, "project"))?;
            own(db, c, string(a, "agent"))?;
        }
        "rename_project_section"
        | "archive_project_section"
        | "restore_project_section"
        | "send_section_message" => {
            let p = member(db, c, string(a, "project"))?;
            if !p["sections"]
                .as_array()
                .is_some_and(|v| v.iter().any(|s| s["id"] == a["section"]))
            {
                return Err(deny());
            }
            if name == "send_section_message"
                && c["kind"] == "session"
                && !messaging::allowed(db, string(c, "turn"))?
            {
                return Err(deny());
            }
        }
        "create_routine" => {
            let agent = a["agent"]
                .as_str()
                .or(c["agent"].as_str())
                .ok_or_else(|| error(400, "Choose a routine owner."))?;
            own(db, c, agent)?;
            if let Some(p) = a["project"].as_str() {
                member(db, c, p)?;
            }
        }
        "read_routine" | "update_routine" | "pause_routine" | "resume_routine" | "run_routine"
        | "delete_routine" => {
            if name == "delete_routine" {
                owner(c)?;
            }
            routine(db, c, string(a, "routine"), name != "read_routine")?;
            if let Some(p) = a["project"].as_str() {
                member(db, c, p)?;
            }
        }
        "read_workflow" | "start_workflow" => workflow(db, c, string(a, "workflow"), false)?,
        "read_workflow_run" => workflow(db, c, string(a, "run"), true)?,
        "list_routines" | "list_workflows" | "list_connected_apps" | "list_mcp_connections" => {
            if let Some(p) = a["project"].as_str().filter(|p| *p != "none") {
                member(db, c, p)?;
            }
            if let Some(agent) = a["agent"].as_str() {
                own(db, c, agent)?;
                if let Some(p) = a["project"].as_str() {
                    if !member(db, c, p)?["members"]
                        .as_array()
                        .is_some_and(|v| v.iter().any(|m| m["agent"] == agent))
                    {
                        return Err(deny());
                    }
                }
            }
            if ["list_connected_apps", "list_mcp_connections"].contains(&name)
                && a["project"].is_null()
                && c["kind"] == "session"
                && project_of(db, &current(db, c)?)?.is_some()
            {
                return Err(deny());
            }
        }
        "delegate_task" | "task_status" | "wait_task" | "cancel_task" => {
            if c["kind"] == "session" {
                let t = current(db, c)?;
                if !messaging::allowed(db, string(c, "turn"))? {
                    return Err(deny());
                }
                if name != "delegate_task" {
                    messaging::status(db, &t, string(a, "taskId"))?;
                }
            } else if name == "delegate_task"
                && one(
                    db,
                    "SELECT id FROM turns WHERE id=? AND status='running'",
                    [string(a, "turn")],
                )?
                .is_none()
            {
                return Err(deny());
            }
        }
        _ => (),
    }
    Ok(())
}
fn page(mut rows: Vec<Value>, a: &Value, key: &str) -> Value {
    rows.retain(|r| string(a, "after").is_empty() || string(r, key) > string(a, "after"));
    rows.sort_by(|a, b| string(a, key).cmp(string(b, key)));
    let limit = a["limit"].as_u64().unwrap_or(20) as usize;
    let next = if rows.len() > limit {
        rows[limit - 1][key].clone()
    } else {
        Value::Null
    };
    rows.truncate(limit);
    json!({"items":rows,"next_after":next})
}
pub(crate) fn api<'a>(
    state: &'a AppState,
    path: &'a str,
    body: Option<&'a Value>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send + 'a>> {
    Box::pin(async move {
        let request = Request::builder()
            .uri(path)
            .method(if body.is_some() {
                Method::POST
            } else {
                Method::GET
            })
            .header("origin", &state.origin)
            .header(
                "host",
                state
                    .origin
                    .split_once("://")
                    .map(|(_, host)| host)
                    .unwrap_or(""),
            )
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                body.map(Value::to_string).unwrap_or_default(),
            ))
            .map_err(|_| Failure::storage())?;
        let response = if path.starts_with("/api/routines")
            || path.starts_with("/api/workflows")
            || path.starts_with("/api/workflow-")
            || path.starts_with("/api/agents/")
                && (path.contains("/routines") || path.contains("/workflows"))
        {
            crate::automation::local(state, request).await?
        } else if path == "/api/machines"
            || path.starts_with("/api/agents/") && path.contains("/computer")
        {
            crate::computer::feature(state, request).await?
        } else if path.contains("/connected-apps") || path.contains("/connected-app-policies") {
            crate::connectors::entry(axum::extract::State(state.clone()), request).await
        } else {
            crate::local(state, request).await?
        };
        let status = response.status().as_u16();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .map_err(|_| error(413, "Feature result exceeds its buffer."))?;
        let v: Value = serde_json::from_slice(&bytes).map_err(|_| Failure::storage())?;
        if status >= 400 {
            return Err(error(status, string(&v, "error")));
        }
        Ok(v)
    })
}
fn command(c: &Value, id: &str) -> String {
    let h = registry::hash(&json!([c["id"], id]).to_string());
    format!(
        "{}-{}-4{}-8{}-{}",
        &h[..8],
        &h[8..12],
        &h[13..16],
        &h[17..20],
        &h[20..32]
    )
}
pub(crate) fn plan(db: &Connection, c: &Value, name: &str, a: &Value, id: &str) -> Result<Value> {
    let mut b = pick(
        a,
        &[
            "name",
            "role",
            "model",
            "instructions",
            "toolBudget",
            "revision",
            "description",
            "folders",
            "members",
            "confirmWrite",
            "prompt",
            "schedule",
            "zone",
            "project",
            "reportTo",
            "enabled",
            "trigger",
            "input",
        ],
    );
    b["id"] = json!(id);
    let path = match name {
        "create_agent" => "/api/agents".into(),
        "update_agent" => {
            let r = data::agents::read(db, string(a, "agent"))?;
            for k in ["name", "role", "model", "instructions"] {
                if b[k].is_null() {
                    b[k] = r[k].clone();
                }
            }
            format!("/api/agents/{}/edit", string(a, "agent"))
        }
        "hide_agent" => {
            b = json!({"id":id,"revision":a["revision"],"hidden":true});
            format!("/api/agents/{}/visibility", string(a, "agent"))
        }
        "create_conversation" => format!("/api/agents/{}/threads", string(a, "agent")),
        "rename_conversation" | "archive_conversation" | "restore_conversation" => {
            let r = one(
                db,
                "SELECT name,archived FROM threads WHERE id=?",
                [string(a, "thread")],
            )?
            .ok_or_else(|| error(404, "Conversation not found."))?;
            b["name"] = a.get("name").unwrap_or(&r["name"]).clone();
            b["archived"] = json!(if name == "rename_conversation" {
                r["archived"] == 1
            } else {
                name == "archive_conversation"
            });
            format!("/api/threads/{}/edit", string(a, "thread"))
        }
        "delete_conversation" => format!("/api/threads/{}/delete", string(a, "thread")),
        "create_project" => {
            for (k, v) in [
                ("description", json!("")),
                ("folders", json!([])),
                ("members", json!([])),
                ("confirmWrite", json!(false)),
            ] {
                if b[k].is_null() {
                    b[k] = v;
                }
            }
            "/api/projects".into()
        }
        "add_project_section" => {
            b = pick(&b, &["id", "revision", "name"]);
            format!("/api/projects/{}/sections", string(a, "project"))
        }
        "rename_project_section" | "archive_project_section" | "restore_project_section" => {
            b = pick(&b, &["id", "revision", "name"]);
            if name != "rename_project_section" {
                b["archived"] = json!(name == "archive_project_section");
            }
            format!(
                "/api/projects/{}/sections/{}/edit",
                string(a, "project"),
                string(a, "section")
            )
        }
        "add_project_member" | "remove_project_member" => {
            let p = member(db, c, string(a, "project"))?;
            let mut members = p["members"].as_array().cloned().unwrap_or_default();
            members.retain(|m| m["agent"] != a["agent"]);
            if name == "add_project_member" {
                members.push(json!({"agent":a["agent"],"write":a["write"]}));
            }
            b = json!({"id":id,"revision":a["revision"],"members":members,"confirmWrite":a["confirmWrite"]==true});
            format!("/api/projects/{}/edit", string(a, "project"))
        }
        "send_section_message" => {
            b = json!({"id":id,"text":a["text"]});
            if a["recipient"].is_string() {
                b["recipient"] = a["recipient"].clone();
            }
            format!("/api/channels/{}/turns", string(a, "section"))
        }
        "create_routine" => format!(
            "/api/agents/{}/routines",
            a["agent"].as_str().unwrap_or_else(|| string(c, "agent"))
        ),
        "update_routine" | "pause_routine" | "resume_routine" => {
            let r = routine(db, c, string(a, "routine"), true)?;
            for (out, key) in [
                ("name", "name"),
                ("prompt", "prompt"),
                ("trigger", "trigger"),
                ("project", "project"),
                ("reportTo", "report_to"),
                ("zone", "schedule_zone"),
            ] {
                if b.get(out).is_none() && !r[key].is_null() {
                    b[out] = r[key].clone();
                }
            }
            b["enabled"] = json!(match name {
                "pause_routine" => false,
                "resume_routine" => true,
                _ => a["enabled"].as_bool().unwrap_or(r["enabled"] == 1),
            });
            if b["schedule"].is_null() {
                if let Some(v) = r["schedule_json"].as_str() {
                    b["schedule"] = serde_json::from_str(v).map_err(|_| Failure::storage())?;
                } else {
                    b["nextAt"] = r["next_at"].clone();
                    b["intervalSeconds"] = r["interval_ms"]
                        .as_f64()
                        .map(|v| json!(v / 1000.))
                        .unwrap_or(Value::Null);
                }
            }
            format!("/api/routines/{}/edit", string(a, "routine"))
        }
        "run_routine" | "delete_routine" => format!(
            "/api/routines/{}/{}",
            string(a, "routine"),
            if name == "run_routine" {
                "test"
            } else {
                "delete"
            }
        ),
        "start_workflow" => format!("/api/workflows/{}/run", string(a, "workflow")),
        "open_computer" => {
            b = json!({});
            format!("/api/agents/{}/computer/start", string(a, "agent"))
        }
        _ => return Err(error(403, "MCP authorization failed.")),
    };
    Ok(json!({"path":path,"body":b}))
}
pub(crate) fn clean(state: &AppState, db: &Connection, v: &Value) -> Result<Value> {
    let mut v = v.clone();
    for a in data::providers::accounts_at(db)? {
        if let Some(key) = a.key.as_deref() {
            v = connections::redact(&v, key);
        }
    }
    for row in all(
        db,
        "SELECT ciphertext FROM composio_settings WHERE ciphertext<>''",
        [],
    )? {
        v = connections::redact(
            &v,
            &crate::connectors::composio::decrypt(state, string(&row, "ciphertext"))?,
        );
    }
    for row in all(db, "SELECT token,url FROM mcp_connections", [])? {
        v = connections::redact(
            &v,
            &client::decrypt(state, string(&row, "token"), string(&row, "url"))?,
        );
    }
    for secret in secrets::project(state, db, None)? {
        v = connections::redact(&v, &secret);
    }
    if v.to_string().len() > 96 * 1024 {
        return Err(error(
            413,
            "Result exceeds 96 KiB. Use a smaller page or text offset.",
        ));
    }
    Ok(v)
}
fn snapshot(db: &Connection, c: &Value, mut p: Value) -> Result<Value> {
    if c["kind"] == "session" {
        let t = current(db, c)?;
        if let Some(rows) = p["threads"].as_array_mut() {
            rows.retain(|r| r["id"] == t["thread"] && r["agent"] == c["agent"]);
        }
    }
    Ok(p)
}
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(o) => {
            let sorted: std::collections::BTreeMap<_, _> =
                o.iter().map(|(k, v)| (k.clone(), canonical(v))).collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(a) => json!(a.iter().map(canonical).collect::<Vec<_>>()),
        value => value.clone(),
    }
}
static IN_FLIGHT: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();
struct InFlight(String);
impl InFlight {
    fn acquire(key: String) -> Result<Self> {
        if !IN_FLIGHT
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| Failure::storage())?
            .insert(key.clone())
        {
            return Err(error(
                409,
                "Request is still running. Retry with the same request ID.",
            ));
        }
        Ok(Self(key))
    }
}
impl Drop for InFlight {
    fn drop(&mut self) {
        if let Ok(mut keys) = IN_FLIGHT.get_or_init(Default::default).lock() {
            keys.remove(&self.0);
        }
    }
}
pub async fn run(state: &AppState, c: &Value, name: &str, a: &Value) -> Result<Value> {
    let _flight = if a["requestId"].is_string() {
        Some(InFlight::acquire(format!(
            "{}:{}:{}",
            state.origin,
            string(c, "id"),
            string(a, "requestId")
        ))?)
    } else {
        None
    };
    let p = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        authorize(state, db, c, name, a)?;
        if a["requestId"].is_string() {
            let mut request = json!({"name":name});
            if let (Some(request), Some(args)) = (request.as_object_mut(), a.as_object()) {
                request.extend(args.clone());
            }
            let digest = registry::hash(&canonical(&request).to_string());
            let old = one(
                db,
                "SELECT * FROM mcp_feature_commands WHERE caller=? AND request_id=?",
                [string(c, "id"), string(a, "requestId")],
            )?;
            if let Some(r) = &old {
                if r["digest"] != digest {
                    return Err(error(
                        409,
                        "Request ID was already used for a different operation.",
                    ));
                }
                if r["result"].is_string() {
                    return data::json(&r["result"]);
                }
            }
            let p = if ["delegate_task", "wait_task", "cancel_task"].contains(&name) {
                Value::Null
            } else if let Some(r) = old.as_ref().filter(|v| v["plan"].is_string()) {
                data::json(&r["plan"])?
            } else {
                plan(db, c, name, a, &command(c, string(a, "requestId")))?
            };
            if old.is_none() {
                db.execute(
                    "INSERT INTO mcp_feature_commands VALUES(?,?,?,?,NULL)",
                    params![
                        string(c, "id"),
                        string(a, "requestId"),
                        digest,
                        if p.is_null() {
                            None
                        } else {
                            Some(p.to_string())
                        }
                    ],
                )?;
            }
            p
        } else {
            Value::Null
        }
    };
    let v = if !p.is_null() {
        api(state, string(&p, "path"), p.get("body")).await?
    } else {
        read(state, c, name, a).await?
    };
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    authorize(state, db, c, name, a)?;
    let v = if ["read_agent", "create_agent", "update_agent", "hide_agent"].contains(&name) {
        pick(&v, AGENT)
    } else if name.ends_with("project_section")
        || name.ends_with("project_member")
        || name == "create_project"
    {
        let mut p = snapshot(db, c, v)?;
        let mut more = false;
        for k in ["members", "sections", "threads"] {
            if let Some(rows) = p[k].as_array_mut() {
                more |= rows.len() > 50;
                rows.truncate(50);
            }
        }
        p["more"] = json!(more);
        p
    } else {
        v
    };
    let v = clean(state, db, &v)?;
    if a["requestId"].is_string() {
        db.execute(
            "UPDATE mcp_feature_commands SET result=? WHERE caller=? AND request_id=?",
            [
                v.to_string(),
                string(c, "id").into(),
                string(a, "requestId").into(),
            ],
        )?;
    }
    Ok(v)
}
pub(crate) async fn read(state: &AppState, c: &Value, name: &str, a: &Value) -> Result<Value> {
    match name {
        "web_fetch" | "web_search" => crate::turns::web::call(state, name, a).await,
        "list_routines" | "read_routine" | "list_workflows" | "read_workflow"
        | "read_workflow_run" => automation(state, c, name, a).await,
        "read_usage" | "list_skills" => usage_skills(state, c, name, a),
        "read_agent" => {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            let mut r = data::agents::read(&guard.connection, string(a, "agent"))?;
            r["status"] = json!(if one(
                &guard.connection,
                "SELECT id FROM turns WHERE agent=? AND status='running'",
                [string(a, "agent")]
            )?
            .is_some()
            {
                "working"
            } else {
                "idle"
            });
            Ok(r)
        }
        "list_projects" | "read_project" => {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            let db = &guard.connection;
            if name == "list_projects" {
                let mut rows = Vec::new();
                for row in all(db, "SELECT id FROM local_projects WHERE deleted=0", [])? {
                    let p = data::projects::read(db, string(&row, "id"))?;
                    if c["kind"] == "peer"
                        || p["members"]
                            .as_array()
                            .is_some_and(|v| v.iter().any(|m| m["agent"] == c["agent"]))
                    {
                        rows.push(pick(
                            &p,
                            &[
                                "id",
                                "name",
                                "description",
                                "revision",
                                "archived",
                                "updated",
                            ],
                        ));
                    }
                }
                return Ok(page(rows, a, "id"));
            }
            let p = snapshot(db, c, member(db, c, string(a, "project"))?)?;
            let mut out = pick(
                &p,
                &[
                    "id",
                    "name",
                    "description",
                    "archived",
                    "revision",
                    "updated",
                    "allowInstallationApps",
                ],
            );
            for k in ["members", "folders", "sections", "threads"] {
                let rows = p[k]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .cloned()
                    .map(|mut r| {
                        if k == "members" {
                            r["id"] = r["agent"].clone();
                        } else if k == "folders" {
                            r = json!({"id":r,"path":r});
                        }
                        r
                    })
                    .collect();
                out[k] = page(rows, a, "id");
            }
            Ok(out)
        }
        "list_conversations" | "read_conversation" => conversations(state, c, name, a),
        "list_machines" => {
            let r = api(state, "/api/machines", None).await?;
            Ok(page(
                r["machines"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|v| {
                        pick(
                            v,
                            &[
                                "id",
                                "name",
                                "status",
                                "version",
                                "online",
                                "lastSeen",
                                "environmentId",
                            ],
                        )
                    })
                    .collect(),
                a,
                "id",
            ))
        }
        "read_update_status" => Ok(pick(
            &api(state, "/api/updates", None).await?,
            &[
                "supported",
                "enabled",
                "current",
                "available",
                "runningTurns",
                "lastAttempt",
                "launcherAvailable",
                "status",
                "version",
                "reason",
            ],
        )),
        "read_computer_status" => Ok(pick(
            &api(
                state,
                &format!("/api/agents/{}/computer", string(a, "agent")),
                None,
            )
            .await?,
            &[
                "enabled",
                "revision",
                "state",
                "available",
                "reason",
                "width",
                "height",
                "control",
            ],
        )),
        "list_project_environment" => {
            let r = api(
                state,
                &format!("/api/projects/{}/environment", string(a, "project")),
                None,
            )
            .await?;
            Ok(page(
                r["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|v| pick(v, &["name", "updated"]))
                    .collect(),
                a,
                "name",
            ))
        }
        "list_mcp_connections" | "list_connected_apps" => integrations(state, c, name, a).await,
        "delegate_task" | "task_status" | "wait_task" | "cancel_task" => {
            delegation(state, c, name, a).await
        }
        _ => Err(error(403, "MCP authorization failed.")),
    }
}
fn conversations(state: &AppState, c: &Value, name: &str, a: &Value) -> Result<Value> {
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    if name == "list_conversations" {
        let mut rows = all(
            db,
            "SELECT id,agent,name,revision,archived FROM threads WHERE agent=? AND deleted=0",
            [string(a, "agent")],
        )?;
        let active = if c["kind"] == "session" {
            Some(current(db, c)?)
        } else {
            None
        };
        rows.retain(|r| {
            a["archived"]
                .as_bool()
                .is_none_or(|v| v == (r["archived"] == 1))
                && active.as_ref().is_none_or(|t| t["thread"] == r["id"])
        });
        for r in &mut rows {
            r["archived"] = json!(r["archived"] == 1);
            r["kind"] = json!("thread");
        }
        if a["archived"] != true
            && active
                .as_ref()
                .is_none_or(|t| t["thread"].is_null() && t["channel"].is_null())
        {
            rows.push(json!({"id":a["agent"],"agent":a["agent"],"name":"Direct conversation","kind":"direct","archived":false,"delivery":false}));
        }
        return Ok(page(rows, a, "id"));
    }
    let cutoff = if c["kind"] == "session" {
        current(db, c)?["seq"].as_i64().unwrap_or(0)
    } else {
        i64::MAX
    };
    let clause = match string(a, "kind") {
        "direct" => "agent=? AND channel IS NULL AND thread IS NULL",
        "thread" => "thread=?",
        _ => "channel=?",
    };
    let limit = a["limit"].as_i64().unwrap_or(10);
    let offset = a["offset"].as_i64().unwrap_or(0);
    let mut rows=all(db,&format!("SELECT id,seq,agent,substr(text,?+1,2000) text,substr(reply,?+1,2000) reply,length(text) text_length,length(reply) reply_length,status,created,paused,revision FROM turns WHERE {clause} AND seq<? AND (? IS NULL OR id=?) ORDER BY seq DESC LIMIT ?"),params![offset,offset,string(a,"conversation"),a["before"].as_i64().unwrap_or(i64::MAX).min(cutoff),a["turn"].as_str(),a["turn"].as_str(),limit+1])?;
    let next = if rows.len() > limit as usize {
        rows[limit as usize - 1]["seq"].clone()
    } else {
        Value::Null
    };
    rows.truncate(limit as usize);
    for r in &mut rows {
        r["next_offset"] = if r["text_length"]
            .as_i64()
            .unwrap_or(0)
            .max(r["reply_length"].as_i64().unwrap_or(0))
            > offset + 2000
        {
            json!(offset + 2000)
        } else {
            Value::Null
        };
    }
    Ok(json!({"turns":rows,"next_before":next}))
}
async fn integrations(state: &AppState, c: &Value, name: &str, a: &Value) -> Result<Value> {
    let prefix = a["project"]
        .as_str()
        .map(|p| format!("/api/projects/{p}"))
        .unwrap_or("/api".into());
    let agent = if c["kind"] == "session" {
        c["agent"].as_str()
    } else {
        a["agent"].as_str()
    };
    if name == "list_connected_apps" {
        let r = api(state, &format!("{prefix}/connected-apps/accounts"), None).await?;
        let mut out = page(
            r["items"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| pick(v, &["id", "toolkit", "status"]))
                .collect(),
            a,
            "id",
        );
        let grants = if let Some(agent) = agent {
            api(
                state,
                &format!("{prefix}/agents/{agent}/connected-apps"),
                None,
            )
            .await?
        } else {
            json!({"items":[]})
        };
        out["grants"] = page(
            grants["items"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| {
                    let mut r = pick(v, &["toolkit", "enabled", "revision"]);
                    r["id"] = v["toolkit"].clone();
                    r
                })
                .collect(),
            a,
            "id",
        );
        return Ok(out);
    }
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("after", string(a, "after"))
        .finish();
    let r = api(state, &format!("{prefix}/mcp?{query}"), None).await?;
    let mut items = Vec::new();
    let limit = a["limit"].as_u64().unwrap_or(20) as usize;
    for v in r["items"].as_array().into_iter().flatten().take(limit) {
        let row = if let Some(agent) = agent {
            api(
                state,
                &format!("{prefix}/mcp/{}?agent={agent}", string(v, "id")),
                None,
            )
            .await?
        } else {
            v.clone()
        };
        items.push(pick(&row, &["id", "name", "enabled", "revision", "grant"]));
    }
    Ok(
        json!({"next_after":if r["items"].as_array().is_some_and(|v|v.len()>items.len()){items.last().map(|v|v["id"].clone()).unwrap_or(Value::Null)}else{r["next_after"].clone()},"items":items}),
    )
}
async fn delegation(state: &AppState, c: &Value, name: &str, a: &Value) -> Result<Value> {
    let turn = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        if c["kind"] == "session" {
            current(db, c)?
        } else if name == "delegate_task" {
            one(db, "SELECT * FROM turns WHERE id=?", [string(a, "turn")])?.ok_or_else(deny)?
        } else {
            one(db,"SELECT t.* FROM delegation_tasks d JOIN turns t ON t.id=d.creator_turn WHERE d.id=?",[string(a,"taskId")])?.ok_or_else(||error(404,"Task not found."))?
        }
    };
    if name == "wait_task" {
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_secs(a["timeoutSeconds"].as_u64().unwrap_or(1));
        loop {
            let status = {
                let guard = state.database.lock().map_err(|_| Failure::storage())?;
                authorize(state, &guard.connection, c, name, a)?;
                messaging::status(&guard.connection, &turn, string(a, "taskId"))?
            };
            if ["done", "failed", "cancelled", "uncertain"].contains(&string(&status, "status"))
                || tokio::time::Instant::now() >= deadline
            {
                return Ok(status);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    if name == "task_status" {
        return messaging::status(db, &turn, string(a, "taskId"));
    }
    let args = if name == "delegate_task" {
        json!({"agent":a["agent"],"task":a["task"],"wait":false})
    } else {
        json!({"taskId":a["taskId"]})
    };
    let call = json!({"id":command(c,string(a,"requestId")),"function":{"name":name,"arguments":args.to_string()}});
    transaction(db, || messaging::run(state, db, &turn, &call, &args))
}
const ROUTINE: &[&str] = &[
    "id",
    "agent",
    "name",
    "prompt",
    "next_at",
    "interval_ms",
    "enabled",
    "revision",
    "trigger",
    "project",
    "report_to",
    "schedule",
    "schedule_zone",
];
async fn automation(state: &AppState, c: &Value, name: &str, a: &Value) -> Result<Value> {
    match name {
        "read_workflow" => Ok(pick(
            &api(
                state,
                &format!("/api/workflows/{}", string(a, "workflow")),
                None,
            )
            .await?,
            &["id", "agent", "name", "revision", "deleted", "graph"],
        )),
        "list_workflows" => {
            let prefix = if c["kind"] == "session" {
                format!("/api/agents/{}/workflows", string(c, "agent"))
            } else if a["agent"].is_string() {
                format!("/api/agents/{}/workflows", string(a, "agent"))
            } else {
                "/api/workflows".into()
            };
            let query = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("after", string(a, "after"))
                .append_pair("deleted", if a["deleted"] == true { "1" } else { "0" })
                .finish();
            let r = api(state, &format!("{prefix}?{query}"), None).await?;
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            let mut rows = Vec::new();
            for w in r["workflows"].as_array().into_iter().flatten() {
                match workflow(&guard.connection, c, string(w, "id"), false) {
                    Ok(()) => rows.push(pick(w, &["id", "agent", "name", "revision", "deleted"])),
                    Err(e) if e.status == 403 => (),
                    Err(e) => return Err(e),
                }
            }
            let mut out = page(rows, a, "id");
            if out["next_after"].is_null() {
                out["next_after"] = r["next_after"].clone();
            }
            Ok(out)
        }
        "read_workflow_run" => {
            let r = api(
                state,
                &format!("/api/workflow-runs/{}", string(a, "run")),
                None,
            )
            .await?;
            let mut out = pick(
                &r,
                &[
                    "id", "workflow", "version", "status", "revision", "cursor", "created",
                ],
            );
            out["steps"] = page(
                r["steps"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|s| {
                        let mut row = pick(
                            s,
                            &[
                                "seq",
                                "node",
                                "status",
                                "task_status",
                                "paused",
                                "agent",
                                "thread",
                            ],
                        );
                        row["id"] = json!(format!("{:010}", s["seq"].as_i64().unwrap_or(0)));
                        row
                    })
                    .collect(),
                a,
                "id",
            );
            Ok(out)
        }
        "read_routine" => {
            let r = {
                let guard = state.database.lock().map_err(|_| Failure::storage())?;
                routine(&guard.connection, c, string(a, "routine"), false)?
            };
            let rows = api(
                state,
                &format!("/api/agents/{}/routines", string(&r, "agent")),
                None,
            )
            .await?;
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            let limit = a["limit"].as_i64().unwrap_or(10);
            let mut runs=all(&guard.connection,"SELECT t.id,t.seq,t.thread,t.status,t.created,substr(t.reply,1,1000) reply FROM routine_runs rr JOIN turns t ON t.id=rr.turn JOIN threads th ON th.id=t.thread WHERE rr.routine=? AND th.deleted=0 AND t.seq<? ORDER BY t.seq DESC LIMIT ?",params![string(a,"routine"),a["before"].as_i64().unwrap_or(i64::MAX),limit+1])?;
            let next = if runs.len() > limit as usize {
                runs[limit as usize - 1]["seq"].clone()
            } else {
                Value::Null
            };
            runs.truncate(limit as usize);
            Ok(
                json!({"routine":pick(rows.as_array().and_then(|v|v.iter().find(|v|v["id"]==a["routine"])).unwrap_or(&Value::Null),ROUTINE),"runs":runs,"next_before":next}),
            )
        }
        "list_routines" => {
            let query = {
                let mut query = url::form_urlencoded::Serializer::new(String::new());
                for k in ["agent", "project", "trigger", "search", "reportTo"] {
                    if let Some(v) = a[k].as_str() {
                        query.append_pair(k, v);
                    }
                }
                if let Some(v) = a["enabled"].as_bool() {
                    query.append_pair("enabled", if v { "1" } else { "0" });
                }
                query.finish()
            };
            let limit = a["limit"].as_u64().unwrap_or(20) as usize;
            let mut cursor = string(a, "after").to_owned();
            let mut items = Vec::new();
            let mut more = false;
            for _ in 0..10 {
                let after = url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("after", &cursor)
                    .finish();
                let r = api(state, &format!("/api/routines?{query}&{after}"), None).await?;
                {
                    let guard = state.database.lock().map_err(|_| Failure::storage())?;
                    for row in r["routines"].as_array().into_iter().flatten() {
                        cursor = string(row, "id").into();
                        match routine(&guard.connection, c, &cursor, false) {
                            Ok(_) => {
                                let mut v = pick(row, ROUTINE);
                                if let Some(v) = v.as_object_mut() {
                                    v.remove("prompt");
                                }
                                items.push(v);
                            }
                            Err(e) if e.status == 403 => (),
                            Err(e) => return Err(e),
                        }
                        if items.len() > limit {
                            break;
                        }
                    }
                }
                more = r["next_after"].is_string();
                if items.len() > limit || !more {
                    break;
                }
            }
            let next = if items.len() > limit {
                items[limit - 1]["id"].clone()
            } else if more {
                json!(cursor)
            } else {
                Value::Null
            };
            items.truncate(limit);
            Ok(json!({"items":items,"next_after":next}))
        }
        _ => Err(error(403, "MCP authorization failed.")),
    }
}
fn usage_skills(state: &AppState, c: &Value, name: &str, a: &Value) -> Result<Value> {
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    if name == "list_skills" {
        let rows = if c["kind"] == "peer" {
            all(db,"SELECT s.id,s.name,substr(v.description,1,1000) description,s.revision FROM skills s JOIN skill_versions v ON v.skill=s.id AND v.revision=s.revision",[])?
        } else {
            all(db,"SELECT s.id,v.name,substr(v.description,1,1000) description,a.version,a.enabled FROM skills s JOIN agent_skills a ON a.skill=s.id JOIN skill_versions v ON v.skill=s.id AND v.revision=a.version WHERE a.agent=? AND a.enabled=1 AND a.removed=0",[string(c,"agent")])?
        };
        return Ok(page(rows, a, "id"));
    }
    let t = if c["kind"] == "session" {
        current(db, c)?
    } else {
        Value::Null
    };
    let limit = a["limit"].as_i64().unwrap_or(20);
    let mut rows=all(db,"SELECT p.seq,p.agent,p.model,p.purpose,p.status,p.created,p.input_tokens,p.output_tokens,p.total_tokens FROM provider_calls p JOIN turns t ON t.id=p.turn WHERE p.seq<? AND (? IS NULL OR (t.agent=? AND t.channel IS ? AND t.thread IS ?)) ORDER BY p.seq DESC LIMIT ?",params![a["before"].as_i64().unwrap_or(i64::MAX),t["agent"].as_str(),t["agent"].as_str(),t["channel"].as_str(),t["thread"].as_str(),limit+1])?;
    let next = if rows.len() > limit as usize {
        rows[limit as usize - 1]["seq"].clone()
    } else {
        Value::Null
    };
    rows.truncate(limit as usize);
    Ok(json!({"calls":rows,"next_before":next}))
}
