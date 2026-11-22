use super::*;
use crate::{
    data::{self, all, closed, exists, integer, one, transaction},
    harness::config::CardWait,
    mcp::validate::valid,
    turns,
};
use rusqlite::params;
use serde::Deserialize;
use std::{collections::HashSet, sync::LazyLock, time::Duration};

const MAX_BYTES: usize = 24_000;
pub const FORM_WAIT_SECONDS: u64 = 900;

static SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("components/schema.json")).unwrap_or_else(|_| unreachable!())
});

#[derive(Deserialize)]
struct Reply {
    tree: Tree,
    data: Map<String, Value>,
    actions: Vec<Action>,
}

#[derive(Deserialize)]
struct Tree {
    children: Vec<Node>,
}

#[derive(Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Bar,
    Line,
    Pie,
    Table,
    Metric,
    Timeline,
    Mermaid,
    Form,
}
impl Kind {
    fn schema_key(self) -> &'static str {
        match self {
            Self::Bar => "bar",
            Self::Line => "line",
            Self::Pie => "pie",
            Self::Table => "table",
            Self::Metric => "metric",
            Self::Timeline => "timeline",
            Self::Mermaid => "mermaid",
            Self::Form => "form",
        }
    }
}

#[derive(Deserialize)]
struct Node {
    #[serde(rename = "type")]
    kind: Kind,
    title: String,
    data: String,
    action: Option<String>,
}

#[derive(Deserialize)]
struct Action {
    id: String,
    label: String,
    context: Map<String, Value>,
}

#[derive(Deserialize)]
struct Table {
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
}

#[derive(Deserialize)]
struct Diagram {
    source: String,
    description: String,
}

#[derive(Deserialize)]
struct Field {
    name: String,
    label: String,
    #[serde(rename = "type")]
    kind: FieldKind,
    #[serde(default)]
    required: bool,
    options: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum FieldKind {
    Text,
    Number,
    Boolean,
    Select,
}

pub fn spec() -> Value {
    json!({"type":"function","function":{
        "name":"render_component",
        "description":"Show native Minarbot UI. Send tree:{type:'stack',children:[{type,title,data:'key'}]}, data keyed by the children's data keys, and actions:[] for static replies. Catalog: bar/line/pie [{label,value:number}]; table {columns:[label],rows:[[cell]]}; metric {value,detail?}; timeline [{label,when?,detail?}]; mermaid {source,description}. Form data [{name,label,type:'text'|'number'|'boolean'|'select',required?,options?:[string]}]. Set the form node's action to one matching actions entry {id,type:'submit',label,context:{...}}. The tool waits for the user; returns status, action, context and typed values on submission. Use forms for necessary input only. Never request secrets.",
        "parameters":*SCHEMA
    }})
}

fn title_ok(title: &str) -> bool {
    !title.trim().is_empty() && !title.chars().any(char::is_control)
}

fn decode<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T> {
    serde_json::from_value(value.clone()).map_err(|_| error(400, "Invalid component payload."))
}

fn validate(args: &Value) -> Result<Reply> {
    if args.to_string().len() > MAX_BYTES {
        return Err(error(413, "Component payload exceeds 24,000 bytes."));
    }
    if !valid(&SCHEMA, args) {
        return Err(error(
            400,
            "Component payload does not match the catalog schema.",
        ));
    }
    let reply: Reply = decode(args)?;
    let mut bindings = HashSet::new();
    let mut has_form = false;
    let mut has_diagram = false;
    for node in &reply.tree.children {
        let value = reply
            .data
            .get(&node.data)
            .ok_or_else(|| error(400, "Component data key is missing."))?;
        bindings.insert(&node.data);
        let key = node.kind.schema_key();
        // Resolve the data schema by its node binding. The advertised schema
        // describes the union because the data model is keyed by the caller.
        let bound_schema = json!({"$ref":format!("#/$defs/{key}"),"$defs":SCHEMA["$defs"]});
        if !valid(&bound_schema, value) || !title_ok(&node.title) {
            return Err(error(400, format!("Invalid {key} data or title.")));
        }
        if node.kind != Kind::Form && node.action.is_some() {
            return Err(error(400, "Only forms can use actions."));
        }
        match node.kind {
            Kind::Table => {
                let table: Table = decode(value)?;
                if table.columns.iter().any(|column| !title_ok(column))
                    || table
                        .rows
                        .iter()
                        .any(|row| row.len() != table.columns.len())
                {
                    return Err(error(400, "Each table row needs one cell per column."));
                }
            }
            Kind::Pie
                if !value.as_array().is_some_and(|points| {
                    points
                        .iter()
                        .any(|point| point["value"].as_f64().is_some_and(|n| n > 0.0))
                }) =>
            {
                return Err(error(400, "A pie chart needs a positive value."));
            }
            Kind::Mermaid => {
                let diagram: Diagram = decode(value)?;
                if has_diagram
                    || diagram.source.trim().is_empty()
                    || !title_ok(&diagram.description)
                    || diagram.source.trim_start().starts_with("---")
                    || diagram.source.contains("%%{")
                {
                    return Err(error(400, "Use one Mermaid diagram with a description and no configuration directives."));
                }
                has_diagram = true;
            }
            Kind::Form => {
                if has_form
                    || !matches!(reply.actions.as_slice(), [action] if node.action.as_ref() == Some(&action.id) && title_ok(&action.label))
                {
                    return Err(error(400, "Use one form with one matching submit action."));
                }
                let fields: Vec<Field> = decode(value)?;
                let mut names = HashSet::new();
                if fields.iter().any(|field| {
                    !names.insert(&field.name)
                        || !title_ok(&field.label)
                        || !match (&field.kind, &field.options) {
                            (FieldKind::Select, Some(options)) => {
                                options.iter().all(|option| title_ok(option))
                            }
                            (FieldKind::Select, None) | (_, Some(_)) => false,
                            (_, None) => true,
                        }
                }) {
                    return Err(error(
                        400,
                        "Form fields need distinct names, labels and valid choices.",
                    ));
                }
                has_form = true;
            }
            _ => {}
        }
    }
    if bindings.len() != reply.data.len() || (!has_form && !reply.actions.is_empty()) {
        return Err(error(
            400,
            "Component data and actions must be used by the tree.",
        ));
    }
    Ok(reply)
}

fn without_secrets(state: &AppState, db: &Connection, raw: &str) -> Result<()> {
    let secrets = turns::diagnostics::secrets(state, db)?;
    if turns::diagnostics::text(raw, &secrets) != raw
        || secrets
            .iter()
            .any(|secret| crate::mcp::secrets::reflected(raw.as_bytes(), secret))
    {
        return Err(error(
            400,
            "Component replies and forms cannot include credentials.",
        ));
    }
    Ok(())
}

fn mermaid_source(diagram: Diagram) -> String {
    let escape = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    include_str!("components/mermaid.html")
        .replacen(
            "<p id=\"description\"></p>",
            &format!("<p id=\"description\">{}</p>", escape(&diagram.description)),
            1,
        )
        .replacen(
            "<pre id=\"fallback\"></pre>",
            &format!("<pre id=\"fallback\">\n{}</pre>", escape(&diagram.source)),
            1,
        )
}

pub async fn run(state: &AppState, turn: &Value, call: &Value, args: &Value) -> Result<Value> {
    if !rich_html::available(state, turn) {
        return Err(error(
            403,
            "Native replies are unavailable for this conversation.",
        ));
    }
    let reply = validate(args)?;
    let turn_id = text(turn, "id");
    let call_id = text(call, "id");
    let id = uuid::Uuid::new_v4().to_string();
    let mut changes = state.events.subscribe();
    let clock = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        without_secrets(state, db, &args.to_string())?;
        if !exists(db,"SELECT 1 FROM turns t LEFT JOIN threads th ON th.id=t.thread LEFT JOIN channels c ON c.id=t.channel WHERE t.id=? AND t.status='running' AND coalesce(th.archived,0)=0 AND coalesce(th.deleted,0)=0 AND coalesce(c.archived,0)=0",[turn_id])? {
            return Err(error(409, "Task stopped or the conversation is archived."));
        }
        if let Some(saved) = recorded(db, turn_id, call_id)? {
            return Ok(saved);
        }
        if exists(
            db,
            "SELECT 1 FROM component_replies WHERE turn=? AND call_id=?",
            [turn_id, call_id],
        )? {
            return Err(error(
                409,
                "This component request is already open or expired.",
            ));
        }
        if db.query_row(
            "SELECT count(*) FROM component_replies WHERE turn=?",
            [turn_id],
            |r| r.get::<_, u64>(0),
        )? >= 4
        {
            return Err(error(429, "A turn can show up to four native replies."));
        }
        let form = reply
            .tree
            .children
            .iter()
            .find(|node| node.kind == Kind::Form);
        if form.is_some()
            && exists(
                db,
                "SELECT 1 FROM harness_approvals WHERE turn=? AND kind='form' AND state='approval'",
                [turn_id],
            )?
        {
            return Err(error(409, "A form is already waiting for input."));
        }
        let clock = if form.is_some() {
            CardWait::start(state, turn_id)?
        } else {
            None
        };
        transaction(db, || {
            let frame = if let Some(node) = reply
                .tree
                .children
                .iter()
                .find(|node| node.kind == Kind::Mermaid)
            {
                let source = mermaid_source(decode(&reply.data[&node.data])?);
                rich_html::publish(
                    state,
                    db,
                    turn,
                    &json!({"id":id}),
                    &json!({"title":node.title,"html":source,"height":180}),
                )?["htmlRender"]
                    .clone()
            } else {
                Value::Null
            };
            let mut payload = args.clone();
            payload["id"] = json!(id);
            payload["frame"] = frame;
            db.execute(
                "INSERT INTO component_replies(id,turn,call_id,payload,frame_id) VALUES(?,?,?,?,?)",
                params![
                    id,
                    turn_id,
                    call_id,
                    payload.to_string(),
                    payload["frame"]["id"].as_str()
                ],
            )?;
            if let Some(node) = form {
                db.execute("INSERT INTO harness_approvals(id,turn,agent,runtime,workspace,kind,description,input,state) VALUES(?,?,?,'','','form',?,'null','approval')",
                    params![id,turn_id,text(turn,"agent"),node.title])?;
            } else {
                turns::harness::journal(db, turn, call, &json!({"status":"rendered","id":id}))?;
            }
            Ok(())
        })?;
        if form.is_none() {
            return Ok(json!({"status":"rendered","id":id}));
        }
        clock
    };

    let _pending = PendingForm {
        state,
        id: &id,
        _clock: clock,
    };
    let wait = async {
        loop {
            {
                let guard = state.database.lock().map_err(|_| Failure::storage())?;
                let db = &guard.connection;
                if let Some(result) = recorded(db, turn_id, call_id)? {
                    return Ok(result);
                }
                if !exists(db,"SELECT 1 FROM harness_approvals a JOIN turns t ON t.id=a.turn WHERE a.id=? AND a.state='approval' AND t.status='running'",[&id])? {
                    return Err(error(409, "Form closed because the task stopped."));
                }
            }
            let _ = changes.recv().await;
        }
    };
    if let Ok(result) = tokio::time::timeout(Duration::from_secs(FORM_WAIT_SECONDS), wait).await {
        return result;
    }
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    transaction(db, || {
        if let Some(result) = recorded(db, turn_id, call_id)? {
            return Ok(result);
        }
        expire(db, &id)?;
        let result = json!({"status":"expired","id":id});
        turns::harness::journal(db, turn, call, &result)?;
        Ok(result)
    })
}

fn expire(db: &Connection, id: &str) -> Result<()> {
    db.execute(
        "UPDATE harness_approvals SET state='expired',revision=revision+1 WHERE id=? AND state='approval'",
        [id],
    )?;
    Ok(())
}

struct PendingForm<'a> {
    state: &'a AppState,
    id: &'a str,
    _clock: Option<CardWait>,
}
impl Drop for PendingForm<'_> {
    fn drop(&mut self) {
        if let Ok(guard) = self.state.database.lock() {
            let _ = expire(&guard.connection, self.id);
        }
    }
}

fn recorded(db: &Connection, turn: &str, call: &str) -> Result<Option<Value>> {
    one(
        db,
        "SELECT result FROM flow_tool_calls WHERE turn=? AND call_id=?",
        [turn, call],
    )?
    .map(|row| data::json(&row["result"]))
    .transpose()
}

fn validate_values(fields: &[Field], values: &Value) -> Result<()> {
    let object = values
        .as_object()
        .ok_or_else(|| error(400, "Submit form values as an object."))?;
    if object
        .keys()
        .any(|key| !fields.iter().any(|field| field.name == *key))
    {
        return Err(error(400, "Form submission contains an unknown field."));
    }
    for field in fields {
        let valid = match (object.get(&field.name), &field.kind) {
            (None, _) => !field.required,
            (Some(Value::String(value)), FieldKind::Text) => {
                value.encode_utf16().count() <= 1000
                    && !value.trim().is_empty()
                    && !value.chars().any(char::is_control)
            }
            (Some(Value::Number(value)), FieldKind::Number) => {
                value.as_f64().is_some_and(|n| n.abs() <= 1e15)
            }
            (Some(Value::Bool(value)), FieldKind::Boolean) => !field.required || *value,
            (Some(Value::String(value)), FieldKind::Select) => field
                .options
                .as_ref()
                .is_some_and(|options| options.contains(value)),
            _ => false,
        };
        if !valid {
            return Err(error(400, format!("Check the value for {}.", field.label)));
        }
    }
    Ok(())
}

pub fn decide(
    state: &AppState,
    db: &Connection,
    turn: &Value,
    approval: &Value,
    v: &Value,
) -> Result<Value> {
    let turn_id = text(turn, "id");
    let id = text(approval, "id");
    let decision = text(v, "decision");
    if !closed(v, &["id", "revision", "decision", "values"])
        || !integer(&v["revision"], 1, 9_007_199_254_740_991)
        || !matches!(
            (decision, v.get("values")),
            ("allow", Some(Value::Object(_))) | ("deny", None)
        )
    {
        return Err(error(400, "Invalid form submission."));
    }
    // Receipts store a digest, keeping values out of the approval command journal.
    let digest = crate::db::digest(&crate::db::canonical(v));
    let request = json!({"turn":turn_id,"approval":id,"digest":digest});
    if let Some(prior) = data::receipt(
        db,
        "harness_decisions",
        text(v, "id"),
        &request,
        "Request ID already used for another approval.",
    )? {
        return Ok(prior);
    }
    if turn["status"] != "running" || approval["state"] != "approval" || approval["revision"] != v["revision"]
        || exists(db,"SELECT 1 FROM turns t LEFT JOIN threads th ON th.id=t.thread LEFT JOIN channels c ON c.id=t.channel WHERE t.id=? AND (th.archived=1 OR th.deleted=1 OR c.archived=1)",[turn_id])?
    {
        return Err(error(409, "Form expired or the conversation is archived."));
    }
    let row = one(
        db,
        "SELECT call_id,payload FROM component_replies WHERE id=? AND turn=?",
        [id, turn_id],
    )?
    .ok_or_else(Failure::storage)?;
    let payload: Value = data::json(&row["payload"])?;
    let reply: Reply = decode(&payload)?;
    let node = reply
        .tree
        .children
        .iter()
        .find(|node| node.kind == Kind::Form)
        .ok_or_else(Failure::storage)?;
    let action = reply.actions.first().ok_or_else(Failure::storage)?;
    let result = if decision == "allow" {
        validate_values(
            &decode::<Vec<Field>>(&reply.data[&node.data])?,
            &v["values"],
        )?;
        without_secrets(state, db, &v["values"].to_string())?;
        json!({"status":"submitted","id":id,"action":action.id,"context":action.context,"values":v["values"]})
    } else {
        json!({"status":"cancelled","id":id,"action":action.id,"context":action.context})
    };
    let call = json!({"id":row["call_id"],"type":"function","function":{"name":"render_component","arguments":"{}"}});
    turns::harness::journal(db, turn, &call, &result)?;
    db.execute(
        "UPDATE harness_approvals SET state='decided',decision=?,revision=revision+1 WHERE id=?",
        [decision, id],
    )?;
    let response = json!({"state":"decided","decision":decision,"revision":approval["revision"].as_i64().unwrap_or(1)+1});
    data::save_receipt(db, "harness_decisions", text(v, "id"), &request, &response)?;
    Ok(response)
}

pub fn references(db: &Connection, turn: &str) -> Result<Vec<Value>> {
    all(
        db,
        "SELECT id,payload,frame_id FROM component_replies WHERE turn=? ORDER BY rowid",
        [turn],
    )
}
