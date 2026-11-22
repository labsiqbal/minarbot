use super::*;

fn binding(account: &data::providers::Account) -> String {
    crate::db::digest(&json!([account.id, account.name, account.family, account.base]).to_string())
}

pub fn request(
    state: &AppState,
    db: &Connection,
    turn: &Value,
    call: &Value,
    args: &Value,
) -> Result<Value> {
    if !closed(args, &["revision", "providerAccount", "model"])
        || !integer(&args["revision"], 1, 9_007_199_254_740_991)
        || !args["providerAccount"].is_string()
        || args["model"].as_str().is_none_or(|s| {
            s.trim().is_empty()
                || s.encode_utf16().count() > 160
                || s.chars().any(|c| c < ' ' || c == '\u{7f}')
        })
    {
        return Err(error(400, "Invalid provider account request."));
    }
    let agent = data::agents::read(db, string(turn, "agent"))?;
    if agent["revision"] != args["revision"] {
        return Err(error(
            409,
            "Profile changed. Reload before choosing an account.",
        ));
    }
    let account = data::providers::accounts(state)?
        .into_iter()
        .find(|a| a.id == args["providerAccount"] && !a.base.is_empty())
        .ok_or_else(|| error(400, "Provider account is unavailable."))?;
    let model = string(args, "model").trim();
    let origin = url::Url::parse(&account.base)
        .map_err(|_| Failure::storage())?
        .origin()
        .ascii_serialization();
    let prompt = format!("Move the agent to account {} ({}), provider {}, destination {}, model {}? History and attachments used while continuing the task will be sent to that provider. The backup model will be disabled. Confirm in Minarbot.", json!(account.name),json!(account.id),account.family,origin,json!(model));
    super::tools::dispatch(
        state,
        db,
        turn,
        &json!({"id":call["id"],"function":{"name":"ask_user"}}),
        &json!({"question":prompt,"choices":["Approve account change","Cancel"]}),
        &mut data::avatar::Prepared::default(),
    )?;
    let question = one(
        db,
        "SELECT id FROM questions WHERE turn=? AND call_id=?",
        [string(turn, "id"), string(call, "id")],
    )?
    .ok_or_else(Failure::storage)?;
    db.execute(
        "UPDATE questions SET call=? WHERE id=?",
        params![call.to_string(), string(&question, "id")],
    )?;
    db.execute(
        "INSERT INTO provider_account_changes VALUES(?,?,?,?,?,?)",
        params![
            string(&question, "id"),
            string(turn, "agent"),
            args["revision"].as_i64(),
            account.id,
            model,
            binding(&account)
        ],
    )?;
    Ok(Value::Null)
}

pub fn confirm(
    state: &AppState,
    db: &Connection,
    question: &str,
    answer: &str,
) -> Result<Option<Value>> {
    let Some(pending) = one(
        db,
        "SELECT * FROM provider_account_changes WHERE question=?",
        [question],
    )?
    else {
        return Ok(None);
    };
    if answer != "Approve account change" {
        return Ok(Some(json!({"changed":false})));
    }
    let account = data::providers::accounts(state)?
        .into_iter()
        .find(|a| a.id == pending["account"] && !a.base.is_empty());
    if account
        .as_ref()
        .is_none_or(|a| binding(a) != pending["binding"])
    {
        return Err(error(
            409,
            "Destination configuration changed. Cancel and request confirmation again.",
        ));
    }
    let agent = data::agents::read(db, string(&pending, "agent"))?;
    if agent["revision"] != pending["revision"] {
        return Err(error(
            409,
            "Profile changed. Cancel and request confirmation again.",
        ));
    }
    if exists(
        db,
        "SELECT 1 FROM turns WHERE agent=? AND status='running'",
        [string(&pending, "agent")],
    )? {
        return Err(error(
            409,
            "Wait for other agent work to finish before changing accounts.",
        ));
    }
    db.execute("UPDATE agents SET provider_account=?,model=?,backup_model='',memory_model='',revision=revision+1 WHERE id=?",params![string(&pending,"account"),string(&pending,"model"),string(&pending,"agent")])?;
    db.execute(
        "DELETE FROM model_fallbacks WHERE agent=?",
        [string(&pending, "agent")],
    )?;
    Ok(Some(
        json!({"changed":true,"providerAccount":pending["account"],"model":pending["model"],"revision":pending["revision"].as_i64().unwrap_or(0)+1}),
    ))
}
