use super::*;

pub fn answer(
    _state: &AppState,
    db: &Connection,
    path: &str,
    conversation: (&str, &str),
    turn: &str,
    question: &str,
    v: &Value,
) -> Result<Value> {
    let target = admission::scoped(
        db,
        conversation.0,
        conversation.1,
        turn,
        "Question not found in this conversation.",
    )?;
    let row = one(
        db,
        "SELECT * FROM questions WHERE id=? AND turn=?",
        params![question, turn],
    )?
    .ok_or_else(|| error(404, "Question not found in this conversation."))?;
    let id = string(v, "id");
    let text = v["text"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= 4000)
        .ok_or_else(|| error(400, "Invalid answer or answer does not match the choices."))?;
    if !closed(v, &["id", "text", "acknowledgeUncertain"])
        || v.get("acknowledgeUncertain")
            .is_some_and(|v| !v.is_boolean())
    {
        return Err(error(
            400,
            "Invalid answer or answer does not match the choices.",
        ));
    }
    let request =
        json!({"path":path,"text":text,"acknowledgeUncertain":v["acknowledgeUncertain"]==true});
    if let Some(prior) = data::receipt(
        db,
        "question_answers",
        id,
        &request,
        "Answer ID was already used.",
    )? {
        return Ok(prior);
    }
    if !target["id"].is_string()
        || !row["answer"].is_null()
        || !["queued", "interrupted"].contains(&string(&target, "status"))
        || target["paused"] != 1
    {
        return Err(error(
            409,
            "Question was already answered, task stopped, or conversation is archived.",
        ));
    }
    if target["status"] == "interrupted" && v["acknowledgeUncertain"] != true {
        return Err(error(
            409,
            "Check previous effects before resuming the interrupted task.",
        ));
    }
    let choices: Value = data::json(&row["choices"])?;
    let choices = choices.as_array().ok_or_else(Failure::storage)?;
    if !choices.is_empty() && !choices.iter().any(|c| c.as_str() == Some(text.trim())) {
        return Err(error(400, "Choose one of the available answers."));
    }
    db.execute(
        "UPDATE questions SET answer=? WHERE id=?",
        params![text.trim(), question],
    )?;
    let result = json!({"id":question,"answer":text.trim()});
    let journal = json!({"answer":text.trim()});
    db.execute("INSERT INTO flow_tool_calls(turn,call_id,call,result,after_output,after_action) VALUES (?,?,?,?,?,?)",params![turn,row["call_id"].as_str(),row["call"].as_str(),journal.to_string(),row["after_output"].as_i64(),row["after_action"].as_i64()])?;
    db.execute(
        "UPDATE turns SET status='queued',paused=0,revision=revision+1,error=NULL WHERE id=?",
        [turn],
    )?;
    data::save_receipt(db, "question_answers", id, &request, &result)?;
    Ok(result)
}
