use super::*;
use std::collections::HashSet;
fn keys(v: &Value, allowed: &[&str]) -> Result<()> {
    if !v.is_object() {
        return Err(error(400, "Workflow object is required."));
    }
    if !data::closed(v, allowed) {
        return Err(error(400, "Unknown workflow field."));
    }
    Ok(())
}
fn text(v: &Value, max: usize, empty: bool) -> Result<String> {
    v.as_str()
        .filter(|s| (empty || !s.trim().is_empty()) && s.encode_utf16().count() <= max)
        .map(str::to_owned)
        .ok_or_else(|| error(400, "Invalid workflow text."))
}
fn id(v: &Value) -> Result<String> {
    let s = text(v, 64, false)?;
    if !s
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err(error(400, "Invalid workflow step ID."));
    }
    Ok(s)
}
fn edges(n: &Value) -> Vec<&str> {
    match string(n, "kind") {
        "output" => vec![],
        "approval" => vec![string(n, "yes")],
        "condition" => vec![string(n, "yes"), string(n, "no")],
        _ => vec![string(n, "next")],
    }
}
fn branch(agents: &HashSet<String>, n: &Value) -> Result<Value> {
    let kind = string(n, "kind");
    if kind == "agent" {
        keys(n, &["kind", "agent", "prompt"])?;
        if !n["agent"].as_str().is_some_and(|a| agents.contains(a)) {
            return Err(error(400, "Workflow agent not found."));
        }
        Ok(json!({"kind":"agent","agent":n["agent"],"prompt":text(&n["prompt"],16000,false)?}))
    } else {
        keys(n, &["kind", "model", "prompt"])?;
        if kind != "model" {
            return Err(error(400, "Parallel branches must use an agent or model."));
        }
        Ok(
            json!({"kind":"model","model":text(&n["model"],200,false)?,"prompt":text(&n["prompt"],16000,false)?}),
        )
    }
}
pub fn validate(db: &Connection, g: &Value) -> Result<Value> {
    validate_scope(db, g, None)
}
pub fn validate_scope(db: &Connection, g: &Value, project: Option<&str>) -> Result<Value> {
    let agents = if let Some(project) = project {
        all(
            db,
            "SELECT agent AS id FROM agent_projects WHERE project=? AND deleted=0",
            [project],
        )?
    } else {
        all(db, "SELECT id FROM agents", [])?
    }
    .iter()
    .map(|a| string(a, "id").to_owned())
    .collect();
    keys(g, &["entry", "maxSteps", "nodes", "positions", "trigger"])?;
    if let Some(trigger) = g.get("trigger") {
        keys(trigger, &["kind", "channel"])?;
        if project.is_some()
            || trigger["kind"] != "discord"
            || !discord_channel(&trigger["channel"])
        {
            return Err(error(
                400,
                "Discord triggers require a channel, thread, or DM ID on a local host.",
            ));
        }
    }
    let entry = id(&g["entry"])?;
    if !integer(&g["maxSteps"], 1, 1000) {
        return Err(error(400, "Workflow limit must be 1 to 1000 steps."));
    }
    let raw = g["nodes"]
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 64)
        .ok_or_else(|| error(400, "Workflow must have 1 to 64 steps."))?;
    let mut nodes = Vec::new();
    for n in raw {
        let name = id(&n["id"])?;
        let kind = string(n, "kind");
        let node = match kind {
            "agent" | "model" => {
                keys(
                    n,
                    if kind == "agent" {
                        &["id", "kind", "agent", "prompt", "next"]
                    } else {
                        &["id", "kind", "model", "prompt", "next"]
                    },
                )?;
                let mut b = n.clone();
                if let Some(o) = b.as_object_mut() {
                    o.remove("id");
                    o.remove("next");
                }
                let parsed = branch(&agents, &b)?;
                let mut x = json!({"id":name,"kind":kind});
                x[if kind == "agent" { "agent" } else { "model" }] =
                    parsed[if kind == "agent" { "agent" } else { "model" }].clone();
                x["prompt"] = parsed["prompt"].clone();
                x["next"] = json!(id(&n["next"])?);
                x
            }
            "parallel" => {
                keys(n, &["id", "kind", "branches", "next"])?;
                let branches = n["branches"]
                    .as_array()
                    .filter(|a| (2..=8).contains(&a.len()))
                    .ok_or_else(|| error(400, "Parallel blocks require 2 to 8 branches."))?;
                json!({"id":name,"kind":kind,"branches":branches.iter().map(|b|branch(&agents,b)).collect::<Result<Vec<_>>>()?,"next":id(&n["next"])?})
            }
            "condition" => {
                let op = string(n, "operator");
                keys(
                    n,
                    if op == "model" {
                        &["id", "kind", "operator", "model", "value", "yes", "no"]
                    } else {
                        &["id", "kind", "operator", "value", "yes", "no"]
                    },
                )?;
                if !["equals", "contains", "model"].contains(&op) {
                    return Err(error(400, "Invalid workflow condition operator."));
                }
                let mut x = json!({"id":name,"kind":kind,"operator":op});
                if op == "model" {
                    x["model"] = json!(text(&n["model"], 200, false)?);
                }
                x["value"] = json!(text(&n["value"], 16000, op != "model")?);
                x["yes"] = json!(id(&n["yes"])?);
                x["no"] = json!(id(&n["no"])?);
                x
            }
            "approval" => {
                keys(n, &["id", "kind", "prompt", "yes", "no"])?;
                let mut x = json!({"id":name,"kind":kind,"prompt":text(&n["prompt"],4000,false)?,"yes":id(&n["yes"])?});
                if n.get("no").is_some() {
                    x["no"] = json!(id(&n["no"])?);
                }
                x
            }
            "wait" => {
                keys(n, &["id", "kind", "event", "timeoutSeconds", "next"])?;
                if !integer(&n["timeoutSeconds"], 1, 604800) {
                    return Err(error(400, "Event wait time must be 1 to 604800 seconds."));
                }
                json!({"id":name,"kind":kind,"event":text(&n["event"],256,false)?,"timeoutSeconds":n["timeoutSeconds"],"next":id(&n["next"])?})
            }
            "output" => {
                keys(n, &["id", "kind", "channel", "discordChannel"])?;
                let mut x = json!({"id":name,"kind":kind});
                if let Some(c) = n.get("channel") {
                    if !c.as_str().is_some_and(uuid) {
                        return Err(error(400, "Invalid output channel."));
                    }
                    x["channel"] = c.clone();
                }
                if let Some(c) = n.get("discordChannel") {
                    if project.is_some() || n.get("channel").is_some() || !discord_channel(c) {
                        return Err(error(400, "Invalid Discord output channel."));
                    }
                    x["discordChannel"] = c.clone();
                }
                x
            }
            _ => return Err(error(400, "Unsupported workflow step type.")),
        };
        nodes.push(node);
    }
    let known: HashSet<_> = nodes.iter().map(|n| string(n, "id")).collect();
    if known.len() != nodes.len()
        || !known.contains(entry.as_str())
        || nodes
            .iter()
            .any(|n| edges(n).iter().any(|e| !known.contains(e)))
    {
        return Err(error(
            400,
            "Duplicate workflow ID or link to a missing step.",
        ));
    }
    let mut reachable = HashSet::from([entry.as_str()]);
    let mut terminating: HashSet<_> = nodes
        .iter()
        .filter(|n| n["kind"] == "output")
        .map(|n| string(n, "id"))
        .collect();
    for _ in &nodes {
        for n in &nodes {
            if reachable.contains(string(n, "id")) {
                reachable.extend(edges(n));
            }
            if edges(n).iter().any(|e| terminating.contains(e)) {
                terminating.insert(string(n, "id"));
            }
        }
    }
    if reachable.len() != nodes.len() || terminating.len() != nodes.len() {
        return Err(error(
            400,
            "Every step must be reachable from the start and have a path to the output.",
        ));
    }
    let mut result = json!({"entry":entry,"maxSteps":g["maxSteps"],"nodes":nodes});
    if let Some(trigger) = g.get("trigger") {
        result["trigger"] = trigger.clone();
    }
    if let Some(p) = g.get("positions") {
        let points = p
            .as_object()
            .ok_or_else(|| error(400, "Workflow object is required."))?;
        for (name, point) in points {
            keys(point, &["x", "y"])?;
            if !known.contains(name.as_str())
                || ["x", "y"].iter().any(|k| {
                    point[*k]
                        .as_f64()
                        .is_none_or(|v| !(64.0..=20000.0).contains(&v))
                })
            {
                return Err(error(400, "Invalid workflow step position."));
            }
        }
        result["positions"] = p.clone();
    }
    Ok(result)
}
fn discord_channel(v: &Value) -> bool {
    v.as_str()
        .is_some_and(crate::connectors::discord::snowflake)
}
pub fn transition(n: &Value, result: &str) -> Result<String> {
    if n["kind"] != "condition" {
        return Ok(string(n, "next").to_owned());
    }
    let yes = match string(n, "operator") {
        "equals" => result == string(n, "value"),
        "contains" => result.contains(string(n, "value")),
        _ => {
            let v = result.trim().to_lowercase();
            if !["yes", "no"].contains(&v.as_str()) {
                return Err(error(400, "Model condition must answer yes or no."));
            }
            v == "yes"
        }
    };
    Ok(string(n, if yes { "yes" } else { "no" }).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_conditions_choose_exact_and_case_sensitive_branches() -> Result<()> {
        let equals = json!({"kind":"condition","operator":"equals","value":"ready","yes":"done","no":"revise"});
        assert_eq!(transition(&equals, "ready")?, "done");
        assert_eq!(transition(&equals, "not ready")?, "revise");

        let mut contains =
            json!({"kind":"condition","operator":"contains","value":"A","yes":"yes","no":"no"});
        assert_eq!(transition(&contains, "DATA")?, "yes");
        contains["value"] = json!("a");
        assert_eq!(transition(&contains, "DATA")?, "no");
        Ok(())
    }
}
