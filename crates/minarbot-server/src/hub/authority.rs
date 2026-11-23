use super::*;

pub(super) struct Context<'a> {
    pub project: &'a str,
    pub account: &'a str,
    pub host: &'a str,
}

pub(super) fn account(db: &Connection, id: &str) -> Result<Value> {
    generation(db)?;
    one(
        db,
        "SELECT * FROM accounts WHERE id=? AND status='active' AND pilot_admitted=1",
        [id],
    )?
    .ok_or_else(|| {
        Failure::new(
            401,
            "account_unavailable",
            "Account is not admitted or active.",
        )
    })
}
pub(super) fn member(db: &Connection, ctx: &Context<'_>, roles: &[&str]) -> Result<Value> {
    account(db, ctx.account)?;
    let entry = one(db, "SELECT m.* FROM memberships m JOIN projects p ON p.id=m.project_id JOIN host_bindings h ON h.project_id=p.id
        WHERE m.project_id=? AND m.account_id=? AND h.host=? AND h.active=1 AND p.status='active' AND m.status='active'",
        [ctx.project, ctx.account, ctx.host])?.ok_or_else(missing)?;
    if !roles.is_empty() && !roles.contains(&text(&entry, "role")) {
        return Err(denied());
    }
    Ok(entry)
}
pub(super) const ADMIN: &[&str] = &["owner", "admin"];
pub(super) const WRITE: &[&str] = &["owner", "admin", "member"];

pub(super) fn context_host(db: &Connection, project: &str) -> Result<String> {
    one(
        db,
        "SELECT host FROM host_bindings WHERE project_id=? AND active=1",
        [project],
    )?
    .map(|row| text(&row, "host").to_owned())
    .ok_or_else(missing)
}
pub(super) fn section(
    db: &Connection,
    ctx: &Context<'_>,
    section: &str,
    roles: &[&str],
) -> Result<Value> {
    member(db, ctx, roles)?;
    one(
        db,
        "SELECT * FROM sections WHERE project_id=? AND id=? AND archived=0",
        [ctx.project, section],
    )?
    .ok_or_else(missing)
}

pub(super) fn create_project(
    hub: &Hub,
    db: &Connection,
    account: &str,
    input: &Value,
) -> Result<Value> {
    closed(input, &["slug", "name"])?;
    let address = text(input, "slug");
    if !slug(address) {
        return Err(Failure::new(
            400,
            "invalid_slug",
            "Choose an available lowercase project address (3-63 characters).",
        ));
    }
    let name = label(&input["name"], 160)?;
    if db
        .prepare("SELECT 1 FROM projects WHERE slug=?")?
        .exists([address])?
    {
        return Err(Failure::new(
            409,
            "slug_unavailable",
            "Project address is unavailable.",
        ));
    }
    let project = new_id();
    let section = new_id();
    let host = format!("{address}.{}", hub.config.domain);
    db.execute(
        "INSERT INTO projects(id,owner_account_id,slug,name) VALUES(?,?,?,?)",
        params![project, account, address, name],
    )?;
    db.execute(
        "INSERT INTO memberships(project_id,account_id,role) VALUES(?,?,'owner')",
        params![project, account],
    )?;
    db.execute(
        "INSERT INTO host_bindings(host,project_id) VALUES(?,?)",
        params![host, project],
    )?;
    db.execute(
        "INSERT INTO sections(id,project_id,name) VALUES(?,?,'general')",
        params![section, project],
    )?;
    Ok(json!({"projectId":project,"sectionId":section,"host":host}))
}

pub(super) fn mailbox(value: &Value) -> Result<String> {
    crate::data::normalize_email(value)
}
fn mailbox_digest(hub: &Hub, value: &Value) -> Result<String> {
    let digest = ring::hmac::sign(&hub.mailbox_key, mailbox(value)?.as_bytes());
    Ok(digest.as_ref().iter().map(|b| format!("{b:02x}")).collect())
}
fn role(value: &Value, allow_revoke: bool) -> Result<&str> {
    value
        .as_str()
        .filter(|v| matches!(*v, "admin" | "member" | "viewer") || allow_revoke && *v == "revoked")
        .ok_or_else(|| Failure::new(400, "invalid_role", "Invalid role."))
}
pub(super) fn invite(
    hub: &Hub,
    db: &Connection,
    ctx: &Context<'_>,
    input: &Value,
) -> Result<Value> {
    closed(input, &["mailbox", "role"])?;
    let actor = member(db, ctx, ADMIN)?;
    let role = role(&input["role"], false)?;
    if role == "admin" && actor["role"] != "owner" {
        return Err(denied());
    }
    let invitation = new_id();
    let token = opaque()?;
    let expires = now() + 86_400_000;
    db.execute("INSERT INTO invitations(id,project_id,issuer_account_id,issuer_revision,mailbox_digest,token_digest,role,expires_at,authority_generation) VALUES(?,?,?,?,?,?,?,?,?)",
        params![invitation, ctx.project, ctx.account, actor["revision"].as_i64(), mailbox_digest(hub, &input["mailbox"])?, digest(&token), role, expires, generation(db)?])?;
    Ok(
        json!({"invitationId":invitation,"expiresAt":expires,"url":format!("https://{}/invite#invitationId={invitation}&token={token}",ctx.host)}),
    )
}
pub(super) fn accept(
    hub: &Hub,
    db: &Connection,
    ctx: &Context<'_>,
    input: &Value,
    identity: &session::Identity,
) -> Result<Value> {
    closed(input, &["invitationId", "token"])?;
    let invitation = id(&input["invitationId"])?;
    let token = text(input, "token");
    let entry = one(
        db,
        "SELECT * FROM invitations WHERE project_id=? AND id=? AND token_digest=?",
        params![ctx.project, invitation, digest(token)],
    )?
    .filter(|_| encoded(token, 32).is_some())
    .ok_or_else(missing)?;
    let verified = identity
        .verified_mailboxes
        .iter()
        .any(|mail| mailbox_digest(hub, &json!(mail)).is_ok_and(|v| entry["mailbox_digest"] == v));
    if !verified
        || entry["target_account_id"]
            .as_str()
            .is_some_and(|v| v != ctx.account)
    {
        return Err(denied());
    }
    if entry["state"] != "pending"
        || entry["expires_at"].as_i64().is_none_or(|v| v <= now())
        || entry["authority_generation"] != generation(db)?
    {
        return Err(Failure::new(
            409,
            "invitation_unavailable",
            "Invitation expired, used, held or revoked.",
        ));
    }
    let issuer = member(
        db,
        &Context {
            account: text(&entry, "issuer_account_id"),
            ..*ctx
        },
        ADMIN,
    )
    .map_err(|e| if e.status < 500 { denied() } else { e })?;
    if issuer["revision"] != entry["issuer_revision"]
        || entry["role"] == "admin" && issuer["role"] != "owner"
    {
        return Err(denied());
    }
    let old = one(
        db,
        "SELECT * FROM memberships WHERE project_id=? AND account_id=?",
        [ctx.project, ctx.account],
    )?
    .unwrap_or(Value::Null);
    if old["status"] == "active" {
        return Err(Failure::new(
            409,
            "already_member",
            "Account is already a member.",
        ));
    }
    db.execute("INSERT INTO memberships(project_id,account_id,role) VALUES(?,?,?)
        ON CONFLICT(project_id,account_id) DO UPDATE SET role=excluded.role,status='active',revision=memberships.revision+1",
        params![ctx.project, ctx.account, text(&entry, "role")])?;
    db.execute(
        "UPDATE invitations SET state='accepted',accepted_by=?,revision=revision+1 WHERE id=?",
        params![ctx.account, invitation],
    )?;
    change(
        db,
        ctx.project,
        "membership.changed",
        ctx.account,
        old["revision"].as_i64().unwrap_or(0) + 1,
    )?;
    Ok(json!({"projectId":ctx.project,"accountId":ctx.account,"role":entry["role"]}))
}
pub(super) fn revoke_invite(db: &Connection, ctx: &Context<'_>, input: &Value) -> Result<Value> {
    closed(input, &["invitationId"])?;
    let m = member(db, ctx, ADMIN)?;
    let id = id(&input["invitationId"])?;
    let invitation = one(
        db,
        "SELECT role FROM invitations WHERE project_id=? AND id=?",
        params![ctx.project, id],
    )?
    .ok_or_else(missing)?;
    if invitation["role"] == "admin" && m["role"] != "owner" {
        return Err(denied());
    }
    db.execute("UPDATE invitations SET state='revoked',revision=revision+1 WHERE project_id=? AND id=? AND state IN('pending','held')", params![ctx.project, id])?;
    Ok(json!({"revoked":true}))
}
pub(super) fn edit_member(db: &Connection, ctx: &Context<'_>, input: &Value) -> Result<Value> {
    closed(input, &["accountId", "revision", "role"])?;
    let actor = member(db, ctx, ADMIN)?;
    let target = id(&input["accountId"])?;
    let existing = one(
        db,
        "SELECT * FROM memberships WHERE project_id=? AND account_id=? AND status='active'",
        params![ctx.project, target],
    )?
    .ok_or_else(missing)?;
    let revision = revision(&input["revision"])?;
    if existing["revision"] != revision {
        return Err(stale());
    }
    let role = role(&input["role"], true)?;
    if existing["role"] == "owner"
        || actor["role"] != "owner" && (existing["role"] == "admin" || role == "admin")
    {
        return Err(denied());
    }
    for p in rows(db, "SELECT p.id FROM participations p JOIN agents a ON a.id=p.agent_id WHERE p.project_id=? AND a.owner_account_id=?", params![ctx.project, target])? {
        invalidate(db, ctx.project, text(&p, "id"))?;
    }
    cancel_turns(db, "requester_account_id", ctx.project, &target)?;
    let (role, status) = if role == "revoked" {
        (text(&existing, "role"), "revoked")
    } else {
        (role, "active")
    };
    db.execute("UPDATE memberships SET role=?,status=?,revision=revision+1 WHERE project_id=? AND account_id=?", params![role, status, ctx.project, target])?;
    change(db, ctx.project, "membership.changed", &target, revision + 1)?;
    Ok(json!({"updated":true}))
}
fn change(db: &Connection, project: &str, kind: &str, subject: &str, revision: i64) -> Result<()> {
    db.execute("INSERT INTO authority_events(project_id,kind,subject_id,revision,created_at) VALUES(?,?,?,?,?)",
        params![project, kind, subject, revision, now()])?;
    Ok(())
}
fn cancel_turns(db: &Connection, column: &str, project: &str, subject: &str) -> Result<()> {
    let sql = format!(
        "SELECT * FROM turns WHERE project_id=? AND {column}=? AND state IN('pending','admitted')"
    );
    for turn in rows(db, &sql, [project, subject])? {
        let turn_id = text(&turn, "id");
        db.execute("UPDATE turns SET state=CASE WHEN state='admitted' THEN 'unknown' ELSE 'revoked' END WHERE id=?", [turn_id])?;
        db.execute("DELETE FROM outbox WHERE turn_id=?", [turn_id])?;
        control(
            db,
            json!({"kind":"turn.cancel","projectId":project,"sectionId":turn["section_id"],"participationId":turn["participation_id"],"nodeId":turn["node_id"],"turnId":turn_id,"revision":0}),
        )?;
    }
    Ok(())
}
fn control(db: &Connection, mut frame: Value) -> Result<()> {
    frame["v"] = json!(1);
    frame["eventId"] = json!(new_id());
    frame["authorityGeneration"] = json!(generation(db)?);
    routing::enqueue(db, &frame, now() + 600_000)
}
fn participation(db: &Connection, project: &str, id: &str) -> Result<Value> {
    one(db, "SELECT p.*,a.owner_account_id,a.home_node_id,a.revision AS agent_revision FROM participations p JOIN agents a ON a.id=p.agent_id WHERE p.project_id=? AND p.id=?", [project, id])?.ok_or_else(missing)
}
fn invalidate(db: &Connection, project: &str, id: &str) -> Result<()> {
    let entry = participation(db, project, id)?;
    let revision = revision(&entry["revision"])? + 1;
    db.execute("UPDATE participations SET state='revoked',revision=?,owner_consent_revision=NULL,project_consent_revision=NULL WHERE project_id=? AND id=?", params![revision, project, id])?;
    // Keep prior cancellation controls until the member node acknowledges them.
    db.execute("DELETE FROM outbox WHERE project_id=? AND participation_id=? AND kind NOT IN('grant.revoked','turn.cancel')", params![project, id])?;
    cancel_turns(db, "participation_id", project, id)?;
    for section in rows(
        db,
        "SELECT section_id FROM participation_sections WHERE project_id=? AND participation_id=?",
        [project, id],
    )? {
        control(
            db,
            json!({"kind":"grant.revoked","projectId":project,"sectionId":section["section_id"],"participationId":id,"nodeId":entry["home_node_id"],"revision":revision}),
        )?;
    }
    change(db, project, "participation.changed", id, revision)
}
pub(super) fn revoke_node(db: &Connection, principal: &Value) -> Result<()> {
    let node = current_node(db, principal)?;
    let node_id = text(&node, "id");
    for p in rows(db, "SELECT p.project_id,p.id FROM participations p JOIN agents a ON a.id=p.agent_id WHERE a.home_node_id=?", [node_id])? {
        invalidate(db, text(&p, "project_id"), text(&p, "id"))?;
    }
    db.execute("UPDATE nodes SET status='revoked',generation=generation+1,connection_generation=connection_generation+1 WHERE id=?", [node_id])?;
    Ok(())
}
pub(super) fn account_status(db: &Connection, account: &str, status: &str) -> Result<()> {
    generation(db)?;
    let row = one(
        db,
        "SELECT revision,status FROM accounts WHERE id=?",
        [account],
    )?
    .ok_or_else(missing)?;
    if row["status"] == status {
        return Ok(());
    }
    for p in rows(db, "SELECT p.project_id,p.id FROM participations p JOIN agents a ON a.id=p.agent_id WHERE a.owner_account_id=?", [account])? {
        invalidate(db, text(&p,"project_id"), text(&p,"id"))?;
    }
    let revision = revision(&row["revision"])? + 1;
    for member in rows(
        db,
        "SELECT project_id FROM memberships WHERE account_id=?",
        [account],
    )? {
        let project = text(&member, "project_id");
        cancel_turns(db, "requester_account_id", project, account)?;
        change(db, project, "account.changed", account, revision)?;
    }
    db.execute("UPDATE nodes SET generation=generation+1,connection_generation=connection_generation+1,status=CASE WHEN status='revoked' THEN status ELSE 'offline' END WHERE owner_account_id=?", [account])?;
    db.execute(
        "UPDATE accounts SET status=?,revision=? WHERE id=?",
        params![status, revision, account],
    )?;
    if status == "blocked" {
        db.execute(
            "DELETE FROM hub_browser_sessions WHERE account_id=?",
            [account],
        )?;
        db.execute(
            "DELETE FROM node_link_sessions WHERE account_id=?",
            [account],
        )?;
        db.execute("DELETE FROM node_link_codes WHERE account_id=?", [account])?;
    }
    Ok(())
}
pub(super) fn withdraw(db: &Connection, ctx: &Context<'_>, input: &Value) -> Result<Value> {
    closed(input, &["participationId"])?;
    let actor = member(db, ctx, &[])?;
    let id = id(&input["participationId"])?;
    let entry = participation(db, ctx.project, &id)?;
    if entry["owner_account_id"] != ctx.account && !ADMIN.contains(&text(&actor, "role")) {
        return Err(denied());
    }
    invalidate(db, ctx.project, &id)?;
    Ok(json!({"withdrawn":true}))
}
pub(super) fn withdraw_node(db: &Connection, principal: &Value, input: &Value) -> Result<()> {
    current_node(db, principal)?;
    let project = id(&input["projectId"])?;
    let id = id(&input["participationId"])?;
    let entry = participation(db, &project, &id)?;
    if entry["home_node_id"] != principal["nodeId"]
        || entry["owner_account_id"] != principal["accountId"]
    {
        return Err(denied());
    }
    let want = revision(&input["revision"])?;
    let current = revision(&entry["revision"])?;
    if current < want {
        return Err(stale());
    }
    if current == want && entry["state"] != "revoked" {
        invalidate(db, &project, &id)?;
    }
    Ok(())
}
fn policy_list(value: &Value, parse: impl Fn(&Value) -> Result<String>) -> Result<Vec<String>> {
    let invalid = || Failure::new(400, "invalid_list", "Invalid or duplicate list entry.");
    let mut entries = value
        .as_array()
        .filter(|a| a.len() <= 100)
        .ok_or_else(invalid)?
        .iter()
        .map(parse)
        .collect::<Result<Vec<_>>>()?;
    entries.sort_unstable();
    if entries.windows(2).any(|p| p[0] == p[1]) {
        return Err(invalid());
    }
    Ok(entries)
}
fn policy(value: &Value) -> Result<Value> {
    closed(
        value,
        &[
            "sectionIds",
            "invocation",
            "scopes",
            "localPolicyDigest",
            "payerBindingRef",
            "maxCostMicros",
        ],
    )?;
    let invalid = || Failure::new(400, "invalid_policy", "Invalid participation policy.");
    let sections = policy_list(&value["sectionIds"], id)?;
    let scopes = policy_list(&value["scopes"], |v| {
        v.as_str()
            .filter(|v| matches!(*v, "read" | "invoke"))
            .map(str::to_owned)
            .ok_or_else(denied)
    })?;
    let payer = text(value, "payerBindingRef");
    let hash = text(value, "localPolicyDigest");
    let ceiling = integer(&value["maxCostMicros"], 0).ok_or_else(invalid)?;
    if sections.is_empty()
        || !scopes.iter().any(|s| s == "read")
        || !hex(hash, 64)
        || payer.is_empty()
        || payer.len() > 128
        || !payer
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err(invalid());
    }
    let invocation = value
        .get("invocation")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| json!({"mode":"owner"}));
    closed(&invocation, &["mode", "accountIds"])?;
    let invocation = match text(&invocation, "mode") {
        "members" => {
            json!({"mode":"members","accountIds":policy_list(&invocation["accountIds"], id)?})
        }
        "owner" | "all-members" if invocation.get("accountIds").is_none() => invocation,
        _ => return Err(invalid()),
    };
    Ok(
        json!({"sectionIds":sections,"invocation":invocation,"scopes":scopes,"localPolicyDigest":hash,"payerBindingRef":payer,"maxCostMicros":ceiling}),
    )
}
pub(super) fn propose(db: &Connection, ctx: &Context<'_>, input: &Value) -> Result<Value> {
    let member = member(db, ctx, WRITE)?;
    let policy = policy(&input["policy"])?;
    let agent = id(&input["agentId"])?;
    let cost = if input["providerMaxCostMicros"].is_null() {
        policy["maxCostMicros"].as_i64()
    } else {
        integer(&input["providerMaxCostMicros"], 1)
    }
    .filter(|v| *v >= 1 && Some(*v) <= policy["maxCostMicros"].as_i64())
    .ok_or_else(|| {
        Failure::new(
            400,
            "invalid_policy",
            "Provider request bound must fit the turn ceiling.",
        )
    })?;
    if !db
        .prepare("SELECT 1 FROM agents WHERE id=? AND owner_account_id=?")?
        .exists(params![agent, ctx.account])?
    {
        return Err(missing());
    }
    let sections = policy["sectionIds"]
        .as_array()
        .ok_or_else(Failure::storage)?;
    for s in sections {
        section(db, ctx, &id(s)?, &[])?;
    }
    if let Some(accounts) = policy["invocation"]["accountIds"].as_array() {
        for account_id in accounts {
            self::member(
                db,
                &Context {
                    account: &id(account_id)?,
                    ..*ctx
                },
                WRITE,
            )?;
        }
    }
    let old = one(
        db,
        "SELECT id,revision FROM participations WHERE project_id=? AND agent_id=?",
        params![ctx.project, agent],
    )?;
    let encoded = canonical(&policy);
    let (id, rev) = if let Some(old) = old {
        let id = text(&old, "id");
        invalidate(db, ctx.project, id)?;
        db.execute("UPDATE participations SET policy_json=?,owner_membership_revision=?,state='proposed' WHERE id=?", params![encoded, member["revision"].as_i64(), id])?;
        db.execute(
            "DELETE FROM participation_sections WHERE project_id=? AND participation_id=?",
            params![ctx.project, id],
        )?;
        (id.to_owned(), revision(&old["revision"])? + 1)
    } else {
        let id = new_id();
        db.execute("INSERT INTO participations(id,project_id,agent_id,owner_membership_revision,policy_json) VALUES(?,?,?,?,?)",
            params![id, ctx.project, agent, member["revision"].as_i64(), encoded])?;
        (id, 1)
    };
    for s in sections {
        db.execute(
            "INSERT INTO participation_sections VALUES(?,?,?)",
            params![ctx.project, id, s.as_str()],
        )?;
    }
    db.execute(
        "INSERT OR REPLACE INTO hub_web_requests VALUES(?,?,?,?)",
        params![id, rev, ctx.account, cost],
    )?;
    Ok(json!({"participationId":id,"revision":rev,"policyDigest":digest(&encoded)}))
}
pub(super) fn consent(
    db: &Connection,
    ctx: &Context<'_>,
    input: &Value,
    principal: Option<&Value>,
) -> Result<()> {
    member(db, ctx, if principal.is_some() { WRITE } else { ADMIN })?;
    let id = id(&input["participationId"])?;
    let rev = revision(&input["revision"])?;
    let p = participation(db, ctx.project, &id)?;
    if p["revision"] != rev
        || p["state"] == "revoked"
        || input["policyDigest"] != digest(text(&p, "policy_json"))
    {
        return Err(stale());
    }
    let owner = text(&p, "owner_account_id");
    let owner_membership = member(
        db,
        &Context {
            account: owner,
            ..*ctx
        },
        WRITE,
    )?;
    if owner_membership["revision"] != p["owner_membership_revision"] {
        return Err(stale());
    }
    if let Some(principal) = principal {
        current_node(db, principal)?;
        if ctx.account != owner
            || principal["accountId"] != owner
            || principal["nodeId"] != p["home_node_id"]
        {
            return Err(denied());
        }
        db.execute(
            "UPDATE participations SET owner_consent_revision=? WHERE project_id=? AND id=?",
            params![rev, ctx.project, id],
        )?;
    } else {
        db.execute(
            "UPDATE participations SET project_consent_revision=? WHERE project_id=? AND id=?",
            params![rev, ctx.project, id],
        )?;
    }
    db.execute("UPDATE participations SET state='active' WHERE project_id=? AND id=? AND owner_consent_revision=revision AND project_consent_revision=revision", params![ctx.project, id])?;
    change(db, ctx.project, "participation.changed", &id, rev)
}
pub(super) fn current_node(db: &Connection, principal: &Value) -> Result<Value> {
    account(db, text(principal, "accountId"))?;
    let node = one(
        db,
        "SELECT * FROM nodes WHERE id=? AND owner_account_id=? AND status!='revoked'",
        [text(principal, "nodeId"), text(principal, "accountId")],
    )?
    .ok_or_else(missing)?;
    if node["generation"] != principal["nodeGeneration"]
        || node["connection_generation"] != principal["connectionGeneration"]
    {
        return Err(stale());
    }
    if node["status"] != "ready" {
        return Err(offline());
    }
    Ok(node)
}
pub(super) fn offline() -> Failure {
    Failure::new(503, "node_offline", "Waiting for member node.")
}

pub(super) struct Grant {
    pub entry: Value,
    pub owner: Value,
    pub membership: Value,
    pub node: Value,
    pub policy: Value,
}

pub(super) fn active(db: &Connection, ctx: &Context<'_>, section: &str, id: &str) -> Result<Grant> {
    let entry = participation(db, ctx.project, id)?;
    if !db.prepare("SELECT 1 FROM participation_sections p JOIN sections s ON s.project_id=p.project_id AND s.id=p.section_id
        WHERE p.project_id=? AND p.participation_id=? AND p.section_id=? AND s.archived=0")?.exists([ctx.project, id, section])? { return Err(missing()); }
    let owner_id = text(&entry, "owner_account_id");
    let owner = account(db, owner_id)?;
    let membership = member(
        db,
        &Context {
            account: owner_id,
            ..*ctx
        },
        WRITE,
    )?;
    if entry["state"] != "active"
        || entry["owner_consent_revision"] != entry["revision"]
        || entry["project_consent_revision"] != entry["revision"]
        || entry["owner_membership_revision"] != membership["revision"]
    {
        return Err(denied());
    }
    let node = one(
        db,
        "SELECT * FROM nodes WHERE id=? AND owner_account_id=? AND status!='revoked'",
        [text(&entry, "home_node_id"), owner_id],
    )?
    .ok_or_else(denied)?;
    let policy = policy(&stored(&entry, "policy_json")?)?;
    Ok(Grant {
        entry,
        owner,
        membership,
        node,
        policy,
    })
}
impl Grant {
    pub(super) fn fence(
        &self,
        db: &Connection,
        ctx: &Context<'_>,
        section_id: &str,
        invoke: bool,
    ) -> Result<Value> {
        section(db, ctx, section_id, if invoke { WRITE } else { &[] })?;
        let scope = if invoke { "invoke" } else { "read" };
        if !self.policy["scopes"]
            .as_array()
            .is_some_and(|v| v.iter().any(|v| v == scope))
        {
            return Err(denied());
        }
        let invocation = &self.policy["invocation"];
        if invoke
            && self.owner["id"] != ctx.account
            && invocation["mode"] != "all-members"
            && !(invocation["mode"] == "members"
                && invocation["accountIds"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|v| v == ctx.account)))
        {
            return Err(denied());
        }
        let requester = account(db, ctx.account)?;
        let member = member(db, ctx, &[])?;
        Ok(
            json!({"authorityGeneration":generation(db)?,"requesterRevision":requester["revision"],"membershipRevision":member["revision"],
            "ownerAccountRevision":self.owner["revision"],"ownerMembershipRevision":self.membership["revision"],"participationRevision":self.entry["revision"],
            "ownerConsentRevision":self.entry["owner_consent_revision"],"projectConsentRevision":self.entry["project_consent_revision"],
            "nodeGeneration":self.node["generation"],"connectionGeneration":self.node["connection_generation"],"keyRevision":self.node["key_revision"],"agentRevision":self.entry["agent_revision"]}),
        )
    }
}
