use super::*;
use authority::Context;
use std::{fs, os::unix::fs::DirBuilderExt};

type Check<T = ()> = crate::db::Result<T>;

fn ok<T>(result: Result<T>) -> Check<T> {
    result.map_err(|error| format!("{}: {}", error.code, error.message).into())
}

fn rejects<T>(result: Result<T>, code: &str) {
    assert_eq!(result.err().map(|error| error.code), Some(code));
}

struct Temp(PathBuf);
impl Temp {
    fn new() -> Check<Self> {
        let dir = std::env::temp_dir().join(format!("minarbot-hub-check-{}", new_id()));
        fs::DirBuilder::new().mode(0o700).create(&dir)?;
        Ok(Self(dir))
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct HubFixture {
    hub: Hub,
    _temp: Temp,
}
impl HubFixture {
    fn new() -> Check<Self> {
        let temp = Temp::new()?;
        let data_path = temp.path("hub.sqlite");
        let (db, writer) = storage::open(&data_path)?;
        let hub = Hub {
            db: Mutex::new(db),
            config: Config {
                domain: "minarbot.test".into(),
                accounts_host: "accounts.minarbot.test".into(),
                data_path,
                mailbox_key_path: temp.path("mailbox"),
                port: 0,
                trusted_gateway_addresses: vec![],
                identity: session::IdentityConfig::Stub { tokens_path: temp.path("tokens") },
                web: None,
                node_link: None,
            },
            mailbox_key: ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &[7; 32]),
            identity: session::Verifier::Stub(HashMap::new()),
            assets: None,
            signer: None,
            limits: Default::default(),
            streams: Arc::new(tokio::sync::Semaphore::new(1)),
            sockets: Arc::new(tokio::sync::Semaphore::new(1)),
            _writer: writer,
        };
        Ok(Self { hub, _temp: temp })
    }

    fn project(&self, db: &mut Connection, owner: &str, slug: &str) -> Check<Value> {
        ok(transaction(db, |db| authority::create_project(
            &self.hub, db, owner, &json!({"slug":slug,"name":slug}),
        )))
    }
}

fn account(db: &Connection, name: &str) -> Check<String> {
    let id = new_id();
    db.execute("INSERT INTO accounts(id,issuer,subject,status,pilot_admitted) VALUES(?,'https://identity.test',?,'active',1)", params![id,name])?;
    Ok(id)
}

fn context<'a>(project: &'a Value, account: &'a str) -> Context<'a> {
    Context { project: text(project, "projectId"), host: text(project, "host"), account }
}

fn join(db: &Connection, project: &Value, account: &str, role: &str) -> Check {
    db.execute("INSERT INTO memberships(project_id,account_id,role) VALUES(?,?,?)", params![text(project,"projectId"),account,role])?;
    Ok(())
}

#[test]
fn roles_fence_reads_posts_and_membership_changes() -> Check {
    let fixture = HubFixture::new()?;
    let mut db = ok(fixture.hub.lock())?;
    let alice = account(&db, "alice")?;
    let bob = account(&db, "bob")?;
    let carol = account(&db, "carol")?;
    let viewer = account(&db, "viewer")?;
    let eve = account(&db, "eve")?;
    let project = fixture.project(&mut db, &alice, "alpha")?;
    for (id, role) in [(&bob,"admin"),(&carol,"member"),(&viewer,"viewer")] {
        join(&db,&project,id,role)?;
    }
    for (id, role) in [(&alice,"owner"),(&bob,"admin"),(&carol,"member"),(&viewer,"viewer")] {
        let ctx = context(&project,id);
        assert_eq!(ok(authority::member(&db,&ctx,&[]))?["role"],role);
        for roles in [authority::WRITE, authority::ADMIN, &["owner"]] {
            let result = authority::member(&db,&ctx,roles);
            if roles.contains(&role) {
                assert_eq!(ok(result)?["role"],role);
            } else {
                rejects(result,"authority_denied");
            }
        }
    }
    let admin = context(&project,&bob);
    rejects(authority::invite(&fixture.hub,&db,&admin,&json!({"mailbox":"eve@example.test","role":"admin"})),"authority_denied");
    for (target, role) in [(&alice,"revoked"),(&bob,"member"),(&carol,"admin")] {
        rejects(authority::edit_member(&db,&admin,&json!({"accountId":target,"revision":1,"role":role})),"authority_denied");
    }
    rejects(routing::post(&db,&context(&project,&viewer),&json!({"requestId":new_id(),"sectionId":project["sectionId"],"text":"No","mentions":[]})),"authority_denied");
    rejects(authority::member(&db,&context(&project,&eve),&[]),"resource_not_found");
    ok(authority::edit_member(&db,&admin,&json!({"accountId":carol,"revision":1,"role":"viewer"})))?;
    assert_eq!(ok(authority::member(&db,&context(&project,&carol),&[]))?["role"],"viewer");
    Ok(())
}

#[test]
fn exact_hosts_and_immutable_project_ownership() -> Check {
    let f = HubFixture::new()?;
    let mut db = ok(f.hub.lock())?;
    let alice = account(&db, "alice")?;
    let bob = account(&db, "bob")?;
    let alpha = f.project(&mut db, &alice, "alpha")?;
    let beta = f.project(&mut db, &bob, "beta")?;
    let ctx = context(&alpha, &alice);
    let section = text(&alpha,"sectionId");
    assert!(crate::web::id(ctx.project).is_some());
    for invalid in ["accounts","api","www","admin","status","assets","UPPER","a","-alpha","alpha-","alpha.beta","alpha:443","équipe"] {
        rejects(transaction(&mut db,|db| authority::create_project(&f.hub,db,&alice,&json!({"slug":invalid,"name":"Invalid"}))),"invalid_slug");
    }
    for host in ["ALPHA.minarbot.test","alpha.minarbot.test:443","alpha.minarbot.test.","unknown.minarbot.test","alpha.minarbot.test@evil.test"] {
        rejects(f.hub.registered(&db,host),"resource_not_found");
    }
    let cross = Context { project: text(&beta,"projectId"), ..ctx };
    rejects(authority::member(&db,&cross,&[]),"resource_not_found");
    rejects(authority::section(&db,&ctx,text(&beta,"sectionId"),&[]),"resource_not_found");
    assert!(db.execute("UPDATE projects SET slug='changed' WHERE id=?", [ctx.project]).is_err());
    assert!(db.execute("UPDATE sections SET project_id=? WHERE id=?", [cross.project,section]).is_err());
    join(&db,&alpha,&bob,"viewer")?;
    rejects(authority::edit_member(&db,&ctx,&json!({"accountId":bob,"revision":1,"role":"owner"})),"invalid_role");
    assert!(db.execute("UPDATE memberships SET role='owner' WHERE project_id=? AND account_id=?", params![ctx.project,bob]).is_err());
    {
        let owner = db.transaction()?;
        owner.execute("UPDATE memberships SET role='admin' WHERE project_id=? AND account_id=?", params![ctx.project,alice])?;
        assert!(owner.commit().is_err());
    }
    assert_eq!(ok(authority::member(&db,&ctx,&[]))?["role"],"owner");
    let owner_count: i64 = db.query_row("SELECT count(*) FROM memberships WHERE project_id=? AND role='owner' AND status='active'",[ctx.project],|row|row.get(0))?;
    assert_eq!(owner_count, 1);
    db.execute("UPDATE projects SET status='deleted' WHERE id=?", [ctx.project])?;
    rejects(f.hub.registered(&db,ctx.host),"resource_not_found");
    rejects(transaction(&mut db,|db| authority::create_project(&f.hub,db,&bob,&json!({"slug":"alpha","name":"Recycled"}))),"slug_unavailable");
    assert!(db.execute("DELETE FROM projects WHERE id=?", [ctx.project]).is_err());
    Ok(())
}

fn identity(subject: &str, mailbox: &str) -> Check<session::Identity> {
    Ok(serde_json::from_value(json!({
        "issuer":"https://identity.test","subject":subject,"sessionId":new_id(),
        "expiresAt":now()+3_600_000,"verifiedMailboxes":[mailbox]
    }))?)
}

fn invite(hub: &Hub, db: &Connection, ctx: &Context<'_>, mailbox: &str, role: &str) -> Check<Value> {
    let invitation = ok(authority::invite(hub,db,ctx,&json!({"mailbox":mailbox,"role":role})))?;
    let expiry = invitation["expiresAt"].as_i64().ok_or("missing invitation expiry")?;
    assert!(expiry <= now()+86_400_000);
    let url = url::Url::parse(text(&invitation,"url"))?;
    let fragment = url.fragment().ok_or("missing invitation proof")?;
    Ok(json!(url::form_urlencoded::parse(fragment.as_bytes()).into_owned().collect::<HashMap<_,_>>()))
}

#[test]
fn invitation_binding_hashes_expiry_replay_and_issuer_membership() -> Check {
    let f = HubFixture::new()?;
    let mut db = ok(f.hub.lock())?;
    let alice = account(&db,"alice")?;
    let bob = account(&db,"bob")?;
    let eve = account(&db,"eve")?;
    let alpha = f.project(&mut db,&alice,"alpha")?;
    let beta = f.project(&mut db,&bob,"beta")?;
    let a = context(&alpha,&alice);
    let b = context(&alpha,&bob);
    let e = context(&alpha,&eve);
    let proof = invite(&f.hub,&db,&a,"BOB@example.test","admin")?;
    let invitation = text(&proof,"invitationId");
    let row = ok(one(&db,"SELECT * FROM invitations WHERE id=?", [invitation]))?.ok_or("invite missing")?;
    assert_ne!(row["token_digest"],proof["token"]);
    assert_ne!(row["mailbox_digest"],"bob@example.test");
    db.execute("UPDATE invitations SET target_account_id=? WHERE id=?", params![bob,invitation])?;
    let bob_identity = identity("bob","bob@example.test")?;
    let eve_identity = identity("eve","eve@example.test")?;
    for mail in ["eve@example.test","bob@example.test"] {
        rejects(authority::accept(&f.hub,&db,&e,&proof,&identity("eve",mail)?),"authority_denied");
    }
    rejects(authority::accept(&f.hub,&db,&b,&proof,&identity("alice","alice@example.test")?),"authority_denied");
    rejects(authority::accept(&f.hub,&db,&context(&beta,&bob),&proof,&bob_identity),"resource_not_found");
    let mut wrong = proof.clone();
    wrong["token"] = json!("wrong");
    rejects(authority::accept(&f.hub,&db,&b,&wrong,&bob_identity),"resource_not_found");
    assert_eq!(ok(authority::accept(&f.hub,&db,&b,&proof,&bob_identity))?["role"],"admin");
    rejects(authority::accept(&f.hub,&db,&b,&proof,&bob_identity),"invitation_unavailable");
    let revoked = invite(&f.hub,&db,&b,"eve@example.test","viewer")?;
    ok(authority::revoke_invite(&db,&a,&json!({"invitationId":revoked["invitationId"]})))?;
    rejects(authority::accept(&f.hub,&db,&e,&revoked,&eve_identity),"invitation_unavailable");
    let pending = invite(&f.hub,&db,&b,"eve@example.test","member")?;
    ok(authority::edit_member(&db,&a,&json!({"accountId":bob,"revision":1,"role":"member"})))?;
    rejects(authority::accept(&f.hub,&db,&e,&pending,&eve_identity),"authority_denied");
    let expired = invite(&f.hub,&db,&a,"eve@example.test","member")?;
    db.execute("UPDATE invitations SET expires_at=? WHERE id=?", params![now(),text(&expired,"invitationId")])?;
    rejects(authority::accept(&f.hub,&db,&e,&expired,&eve_identity),"invitation_unavailable");
    Ok(())
}

struct Node {
    id: String,
    agent: String,
    principal: Value,
}
fn node(db: &Connection, owner: &str) -> Check<Node> {
    let id = new_id();
    let agent = new_id();
    db.execute("INSERT INTO nodes(id,owner_account_id,installation_id,signing_public_key,encryption_public_key,capabilities_json,connection_generation,status) VALUES(?,?,?,'fixture','fixture','[\"chat\"]',1,'ready')",params![id,owner,new_id()])?;
    db.execute("INSERT INTO agents(id,owner_account_id,home_node_id,name) VALUES(?,?,?,'Fixture')",params![agent,owner,id])?;
    let principal = json!({"nodeId":id,"accountId":owner,"nodeGeneration":1,"connectionGeneration":1});
    Ok(Node { id, agent, principal })
}

fn proposal(section: &str, node: &Node, invocation: Value) -> Value {
    json!({"agentId":node.agent,"providerMaxCostMicros":1,"policy":{"sectionIds":[section],"invocation":invocation,"scopes":["read","invoke"],
        "localPolicyDigest":digest("fixture policy"),"payerBindingRef":"node-local-payer","maxCostMicros":100}})
}

fn grant(db: &Connection, project: &Value, owner: &str, member: &str, node: &Node, invocation: Value) -> Check<Value> {
    let ctx = context(project,member);
    let proposal = ok(authority::propose(db,&ctx,&proposal(text(project,"sectionId"),node,invocation)))?;
    ok(authority::consent(db,&context(project,owner),&proposal,None))?;
    ok(authority::consent(db,&ctx,&proposal,Some(&node.principal)))?;
    Ok(proposal)
}

fn message(section: &str, mentions: Value) -> Value {
    json!({"requestId":new_id(),"sectionId":section,"text":"Hello","mentions":mentions})
}

fn post(db: &mut Connection, ctx: &Context<'_>, message: &Value) -> Result<Value> {
    transaction(db,|db| routing::post(db,ctx,message))
}

fn named(section: &str, proposal: &Value) -> Value {
    message(section,json!([proposal["participationId"]]))
}

fn request(db: &Connection, node: &Node) -> Check<Value> {
    ok(routing::pending(db,&node.principal))?.into_iter().find(|frame|frame["kind"]=="turn.request").ok_or_else(||"missing turn request".into())
}

fn result(request: &Value, kind: &str, sequence: i64, payload: Value) -> Value {
    let mut frame = request.clone();
    frame["kind"] = json!(kind);
    frame["eventId"] = json!(new_id());
    frame["sequence"] = json!(sequence);
    frame["payloadDigest"] = json!(digest(&canonical(&payload)));
    frame["payload"] = payload;
    frame
}

fn accept(db: &Connection, node: &Node, frame: &Value) -> Result<Value> {
    routing::accept(db,&node.principal,frame)
}

#[test]
fn fixed_home_and_exact_dual_consent() -> Check {
    let f = HubFixture::new()?;
    let mut db = ok(f.hub.lock())?;
    let alice = account(&db,"alice")?;
    let bob = account(&db,"bob")?;
    let alpha = f.project(&mut db,&alice,"alpha")?;
    let section = text(&alpha,"sectionId");
    let a = context(&alpha,&alice);
    let b = context(&alpha,&bob);
    join(&db,&alpha,&bob,"member")?;
    let n = node(&db,&bob)?;
    let other = node(&db,&alice)?;
    let mut impostor = n.principal.clone();
    impostor["accountId"] = json!(alice);
    rejects(authority::current_node(&db,&impostor),"resource_not_found");
    rejects(authority::revoke_node(&db,&impostor),"resource_not_found");
    let input = proposal(section,&n,json!({"mode":"owner"}));
    rejects(authority::propose(&db,&a,&input),"resource_not_found");
    assert!(db.execute("UPDATE agents SET owner_account_id=? WHERE id=?",params![alice,n.agent]).is_err());
    assert!(db.execute("UPDATE agents SET home_node_id=? WHERE id=?",params![other.id,n.agent]).is_err());
    assert!(db.execute("UPDATE nodes SET owner_account_id=? WHERE id=?",params![alice,n.id]).is_err());

    let p = ok(authority::propose(&db,&b,&input))?;
    rejects(post(&mut db,&b,&named(section,&p)),"authority_denied");
    ok(authority::consent(&db,&a,&p,None))?;
    rejects(post(&mut db,&b,&named(section,&p)),"authority_denied");
    rejects(authority::consent(&db,&a,&p,Some(&other.principal)),"authority_denied");
    rejects(authority::consent(&db,&b,&p,None),"authority_denied");
    let mut wrong = p.clone();
    wrong["policyDigest"] = json!(digest("changed"));
    rejects(authority::consent(&db,&b,&wrong,Some(&n.principal)),"stale_revision");
    ok(authority::consent(&db,&b,&p,Some(&n.principal)))?;
    assert_eq!(ok(post(&mut db,&b,&named(section,&p)))?["turnIds"].as_array().ok_or("missing turn IDs")?.len(),1);
    let wider = ok(authority::propose(&db,&b,&proposal(section,&n,json!({"mode":"all-members"}))))?;
    assert!(wider["revision"].as_i64() > p["revision"].as_i64());
    rejects(post(&mut db,&a,&named(section,&p)),"authority_denied");
    rejects(authority::consent(&db,&b,&p,Some(&n.principal)),"stale_revision");
    Ok(())
}
