use super::*;
use authority::Context;
use std::{
    cell::Cell,
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
};

type Check<T = ()> = crate::db::Result<T>;

thread_local! { static CLOCK: Cell<Option<i64>> = const { Cell::new(None) }; }

pub(super) fn clock() -> Option<i64> {
    CLOCK.get()
}

fn advance(ms: i64) {
    CLOCK.set(Some(now() + ms));
}

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
impl Drop for HubFixture {
    fn drop(&mut self) {
        CLOCK.set(None);
    }
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
                identity: session::IdentityConfig::Stub {
                    tokens_path: temp.path("tokens"),
                },
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
        ok(transaction(db, |db| {
            authority::create_project(&self.hub, db, owner, &json!({"slug":slug,"name":slug}))
        }))
    }
}

fn account(db: &Connection, name: &str) -> Check<String> {
    let id = new_id();
    db.execute("INSERT INTO accounts(id,issuer,subject,status,pilot_admitted) VALUES(?,'https://identity.test',?,'active',1)", params![id,name])?;
    Ok(id)
}

fn context<'a>(project: &'a Value, account: &'a str) -> Context<'a> {
    Context {
        project: text(project, "projectId"),
        host: text(project, "host"),
        account,
    }
}

fn join(db: &Connection, project: &Value, account: &str, role: &str) -> Check {
    db.execute(
        "INSERT INTO memberships(project_id,account_id,role) VALUES(?,?,?)",
        params![text(project, "projectId"), account, role],
    )?;
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
    for (id, role) in [(&bob, "admin"), (&carol, "member"), (&viewer, "viewer")] {
        join(&db, &project, id, role)?;
    }
    for (id, role) in [
        (&alice, "owner"),
        (&bob, "admin"),
        (&carol, "member"),
        (&viewer, "viewer"),
    ] {
        let ctx = context(&project, id);
        assert_eq!(ok(authority::member(&db, &ctx, &[]))?["role"], role);
        for roles in [authority::WRITE, authority::ADMIN, &["owner"]] {
            let result = authority::member(&db, &ctx, roles);
            if roles.contains(&role) {
                assert_eq!(ok(result)?["role"], role);
            } else {
                rejects(result, "authority_denied");
            }
        }
    }
    let admin = context(&project, &bob);
    rejects(
        authority::invite(
            &fixture.hub,
            &db,
            &admin,
            &json!({"mailbox":"eve@example.test","role":"admin"}),
        ),
        "authority_denied",
    );
    for (target, role) in [(&alice, "revoked"), (&bob, "member"), (&carol, "admin")] {
        rejects(
            authority::edit_member(
                &db,
                &admin,
                &json!({"accountId":target,"revision":1,"role":role}),
            ),
            "authority_denied",
        );
    }
    rejects(
        routing::post(
            &db,
            &context(&project, &viewer),
            &json!({"requestId":new_id(),"sectionId":project["sectionId"],"text":"No","mentions":[]}),
        ),
        "authority_denied",
    );
    rejects(
        authority::member(&db, &context(&project, &eve), &[]),
        "resource_not_found",
    );
    ok(authority::edit_member(
        &db,
        &admin,
        &json!({"accountId":carol,"revision":1,"role":"viewer"}),
    ))?;
    assert_eq!(
        ok(authority::member(&db, &context(&project, &carol), &[]))?["role"],
        "viewer"
    );
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
    let section = text(&alpha, "sectionId");
    assert!(crate::web::id(ctx.project).is_some());
    for invalid in [
        "accounts",
        "api",
        "www",
        "admin",
        "status",
        "assets",
        "UPPER",
        "a",
        "-alpha",
        "alpha-",
        "alpha.beta",
        "alpha:443",
        "équipe",
    ] {
        rejects(
            transaction(&mut db, |db| {
                authority::create_project(
                    &f.hub,
                    db,
                    &alice,
                    &json!({"slug":invalid,"name":"Invalid"}),
                )
            }),
            "invalid_slug",
        );
    }
    for host in [
        "ALPHA.minarbot.test",
        "alpha.minarbot.test:443",
        "alpha.minarbot.test.",
        "unknown.minarbot.test",
        "alpha.minarbot.test@evil.test",
    ] {
        rejects(f.hub.registered(&db, host), "resource_not_found");
    }
    let cross = Context {
        project: text(&beta, "projectId"),
        ..ctx
    };
    rejects(authority::member(&db, &cross, &[]), "resource_not_found");
    rejects(
        authority::section(&db, &ctx, text(&beta, "sectionId"), &[]),
        "resource_not_found",
    );
    assert!(db
        .execute(
            "UPDATE projects SET slug='changed' WHERE id=?",
            [ctx.project]
        )
        .is_err());
    assert!(db
        .execute(
            "UPDATE sections SET project_id=? WHERE id=?",
            [cross.project, section]
        )
        .is_err());
    join(&db, &alpha, &bob, "viewer")?;
    rejects(
        authority::edit_member(
            &db,
            &ctx,
            &json!({"accountId":bob,"revision":1,"role":"owner"}),
        ),
        "invalid_role",
    );
    assert!(db
        .execute(
            "UPDATE memberships SET role='owner' WHERE project_id=? AND account_id=?",
            params![ctx.project, bob]
        )
        .is_err());
    {
        let owner = db.transaction()?;
        owner.execute(
            "UPDATE memberships SET role='admin' WHERE project_id=? AND account_id=?",
            params![ctx.project, alice],
        )?;
        assert!(owner.commit().is_err());
    }
    assert_eq!(ok(authority::member(&db, &ctx, &[]))?["role"], "owner");
    let owner_count: i64 = db.query_row(
        "SELECT count(*) FROM memberships WHERE project_id=? AND role='owner' AND status='active'",
        [ctx.project],
        |row| row.get(0),
    )?;
    assert_eq!(owner_count, 1);
    {
        let transfer = db.transaction()?;
        transfer.execute(
            "UPDATE memberships SET role='admin' WHERE project_id=? AND account_id=?",
            params![ctx.project, alice],
        )?;
        transfer.execute(
            "UPDATE memberships SET role='owner' WHERE project_id=? AND account_id=?",
            params![ctx.project, bob],
        )?;
        transfer.execute(
            "UPDATE projects SET owner_account_id=? WHERE id=?",
            params![bob, ctx.project],
        )?;
        transfer.commit()?;
    }
    assert_eq!(ok(authority::member(&db, &ctx, &[]))?["role"], "admin");
    assert_eq!(
        ok(authority::member(&db, &context(&alpha, &bob), &[]))?["role"],
        "owner"
    );
    db.execute(
        "UPDATE projects SET status='deleted' WHERE id=?",
        [ctx.project],
    )?;
    rejects(f.hub.registered(&db, ctx.host), "resource_not_found");
    rejects(
        transaction(&mut db, |db| {
            authority::create_project(&f.hub, db, &bob, &json!({"slug":"alpha","name":"Recycled"}))
        }),
        "slug_unavailable",
    );
    assert!(db
        .execute("DELETE FROM projects WHERE id=?", [ctx.project])
        .is_err());
    Ok(())
}

fn identity(subject: &str, mailbox: &str) -> Check<session::Identity> {
    Ok(serde_json::from_value(json!({
        "issuer":"https://identity.test","subject":subject,"sessionId":new_id(),
        "expiresAt":now()+3_600_000,"verifiedMailboxes":[mailbox]
    }))?)
}

fn invite(
    hub: &Hub,
    db: &Connection,
    ctx: &Context<'_>,
    mailbox: &str,
    role: &str,
) -> Check<Value> {
    let invitation = ok(authority::invite(
        hub,
        db,
        ctx,
        &json!({"mailbox":mailbox,"role":role}),
    ))?;
    let expiry = invitation["expiresAt"]
        .as_i64()
        .ok_or("missing invitation expiry")?;
    assert!(expiry <= now() + 86_400_000);
    let url = url::Url::parse(text(&invitation, "url"))?;
    let fragment = url.fragment().ok_or("missing invitation proof")?;
    Ok(json!(url::form_urlencoded::parse(fragment.as_bytes())
        .into_owned()
        .collect::<HashMap<_, _>>()))
}

#[test]
fn invitation_binding_hashes_expiry_replay_and_issuer_membership() -> Check {
    let f = HubFixture::new()?;
    let mut db = ok(f.hub.lock())?;
    let alice = account(&db, "alice")?;
    let bob = account(&db, "bob")?;
    let eve = account(&db, "eve")?;
    let alpha = f.project(&mut db, &alice, "alpha")?;
    let beta = f.project(&mut db, &bob, "beta")?;
    let a = context(&alpha, &alice);
    let b = context(&alpha, &bob);
    let e = context(&alpha, &eve);
    let proof = invite(&f.hub, &db, &a, "BOB@example.test", "admin")?;
    let invitation = text(&proof, "invitationId");
    let row = ok(one(
        &db,
        "SELECT * FROM invitations WHERE id=?",
        [invitation],
    ))?
    .ok_or("invite missing")?;
    assert_ne!(row["token_digest"], proof["token"]);
    assert_ne!(row["mailbox_digest"], "bob@example.test");
    db.execute(
        "UPDATE invitations SET target_account_id=? WHERE id=?",
        params![bob, invitation],
    )?;
    let bob_identity = identity("bob", "bob@example.test")?;
    let eve_identity = identity("eve", "eve@example.test")?;
    for mail in ["eve@example.test", "bob@example.test"] {
        rejects(
            authority::accept(&f.hub, &db, &e, &proof, &identity("eve", mail)?),
            "authority_denied",
        );
    }
    rejects(
        authority::accept(
            &f.hub,
            &db,
            &b,
            &proof,
            &identity("alice", "alice@example.test")?,
        ),
        "authority_denied",
    );
    rejects(
        authority::accept(&f.hub, &db, &context(&beta, &bob), &proof, &bob_identity),
        "resource_not_found",
    );
    let mut wrong = proof.clone();
    wrong["token"] = json!("wrong");
    rejects(
        authority::accept(&f.hub, &db, &b, &wrong, &bob_identity),
        "resource_not_found",
    );
    assert_eq!(
        ok(authority::accept(&f.hub, &db, &b, &proof, &bob_identity))?["role"],
        "admin"
    );
    rejects(
        authority::accept(&f.hub, &db, &b, &proof, &bob_identity),
        "invitation_unavailable",
    );
    let revoked = invite(&f.hub, &db, &b, "eve@example.test", "viewer")?;
    ok(authority::revoke_invite(
        &db,
        &a,
        &json!({"invitationId":revoked["invitationId"]}),
    ))?;
    rejects(
        authority::accept(&f.hub, &db, &e, &revoked, &eve_identity),
        "invitation_unavailable",
    );
    let pending = invite(&f.hub, &db, &b, "eve@example.test", "member")?;
    ok(authority::edit_member(
        &db,
        &a,
        &json!({"accountId":bob,"revision":1,"role":"member"}),
    ))?;
    rejects(
        authority::accept(&f.hub, &db, &e, &pending, &eve_identity),
        "authority_denied",
    );
    let expired = invite(&f.hub, &db, &a, "eve@example.test", "member")?;
    db.execute(
        "UPDATE invitations SET expires_at=? WHERE id=?",
        params![now(), text(&expired, "invitationId")],
    )?;
    rejects(
        authority::accept(&f.hub, &db, &e, &expired, &eve_identity),
        "invitation_unavailable",
    );
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
    db.execute(
        "INSERT INTO agents(id,owner_account_id,home_node_id,name) VALUES(?,?,?,'Fixture')",
        params![agent, owner, id],
    )?;
    let principal =
        json!({"nodeId":id,"accountId":owner,"nodeGeneration":1,"connectionGeneration":1});
    Ok(Node {
        id,
        agent,
        principal,
    })
}

fn proposal(section: &str, node: &Node, invocation: Value) -> Value {
    json!({"agentId":node.agent,"providerMaxCostMicros":1,"policy":{"sectionIds":[section],"invocation":invocation,"scopes":["read","invoke"],
        "localPolicyDigest":digest("fixture policy"),"payerBindingRef":"node-local-payer","maxCostMicros":100}})
}

fn grant(
    db: &Connection,
    project: &Value,
    owner: &str,
    member: &str,
    node: &Node,
    invocation: Value,
) -> Check<Value> {
    let ctx = context(project, member);
    let proposal = ok(authority::propose(
        db,
        &ctx,
        &proposal(text(project, "sectionId"), node, invocation),
    ))?;
    ok(authority::consent(
        db,
        &context(project, owner),
        &proposal,
        None,
    ))?;
    ok(authority::consent(
        db,
        &ctx,
        &proposal,
        Some(&node.principal),
    ))?;
    Ok(proposal)
}

fn message(section: &str, mentions: Value) -> Value {
    json!({"requestId":new_id(),"sectionId":section,"text":"Hello","mentions":mentions})
}

fn post(db: &mut Connection, ctx: &Context<'_>, message: &Value) -> Result<Value> {
    transaction(db, |db| routing::post(db, ctx, message))
}

fn named(section: &str, proposal: &Value) -> Value {
    message(section, json!([proposal["participationId"]]))
}

fn request(db: &Connection, node: &Node) -> Check<Value> {
    ok(routing::pending(db, &node.principal))?
        .into_iter()
        .find(|frame| frame["kind"] == "turn.request")
        .ok_or_else(|| "missing turn request".into())
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
    routing::accept(db, &node.principal, frame)
}

#[test]
fn fixed_home_and_exact_dual_consent() -> Check {
    let f = HubFixture::new()?;
    let mut db = ok(f.hub.lock())?;
    let alice = account(&db, "alice")?;
    let bob = account(&db, "bob")?;
    let alpha = f.project(&mut db, &alice, "alpha")?;
    let section = text(&alpha, "sectionId");
    let a = context(&alpha, &alice);
    let b = context(&alpha, &bob);
    join(&db, &alpha, &bob, "member")?;
    let n = node(&db, &bob)?;
    let other = node(&db, &alice)?;
    let mut impostor = n.principal.clone();
    impostor["accountId"] = json!(alice);
    rejects(
        authority::current_node(&db, &impostor),
        "resource_not_found",
    );
    rejects(authority::revoke_node(&db, &impostor), "resource_not_found");
    let input = proposal(section, &n, json!({"mode":"owner"}));
    rejects(authority::propose(&db, &a, &input), "resource_not_found");
    assert!(db
        .execute(
            "UPDATE agents SET owner_account_id=? WHERE id=?",
            params![alice, n.agent]
        )
        .is_err());
    assert!(db
        .execute(
            "UPDATE agents SET home_node_id=? WHERE id=?",
            params![other.id, n.agent]
        )
        .is_err());
    assert!(db
        .execute(
            "UPDATE nodes SET owner_account_id=? WHERE id=?",
            params![alice, n.id]
        )
        .is_err());

    let p = ok(authority::propose(&db, &b, &input))?;
    rejects(post(&mut db, &b, &named(section, &p)), "authority_denied");
    ok(authority::consent(&db, &a, &p, None))?;
    rejects(post(&mut db, &b, &named(section, &p)), "authority_denied");
    rejects(
        authority::consent(&db, &a, &p, Some(&other.principal)),
        "authority_denied",
    );
    rejects(authority::consent(&db, &b, &p, None), "authority_denied");
    let mut wrong = p.clone();
    wrong["policyDigest"] = json!(digest("changed"));
    rejects(
        authority::consent(&db, &b, &wrong, Some(&n.principal)),
        "stale_revision",
    );
    ok(authority::consent(&db, &b, &p, Some(&n.principal)))?;
    assert_eq!(
        ok(post(&mut db, &b, &named(section, &p)))?["turnIds"]
            .as_array()
            .ok_or("missing turn IDs")?
            .len(),
        1
    );
    let wider = ok(authority::propose(
        &db,
        &b,
        &proposal(section, &n, json!({"mode":"all-members"})),
    ))?;
    assert!(wider["revision"].as_i64() > p["revision"].as_i64());
    rejects(post(&mut db, &a, &named(section, &p)), "authority_denied");
    rejects(
        authority::consent(&db, &b, &p, Some(&n.principal)),
        "stale_revision",
    );
    Ok(())
}

struct Pair {
    alice: String,
    bob: String,
    eve: String,
    alpha: Value,
    beta: Value,
    owner: Node,
    peer: Node,
}
impl Pair {
    fn new(f: &HubFixture, db: &mut Connection) -> Check<Self> {
        let alice = account(db, "alice")?;
        let bob = account(db, "bob")?;
        let eve = account(db, "eve")?;
        let alpha = f.project(db, &alice, "alpha")?;
        let beta = f.project(db, &bob, "beta")?;
        join(db, &alpha, &bob, "member")?;
        let owner = node(db, &alice)?;
        let peer = node(db, &bob)?;
        Ok(Self {
            alice,
            bob,
            eve,
            alpha,
            beta,
            owner,
            peer,
        })
    }

    fn alice(&self) -> Context<'_> {
        context(&self.alpha, &self.alice)
    }

    fn bob(&self) -> Context<'_> {
        context(&self.alpha, &self.bob)
    }

    fn section(&self) -> &str {
        text(&self.alpha, "sectionId")
    }

    fn own_grant(&self, db: &Connection) -> Check<Value> {
        grant(
            db,
            &self.alpha,
            &self.alice,
            &self.alice,
            &self.owner,
            json!({"mode":"owner"}),
        )
    }

    fn peer_grant(&self, db: &Connection, invocation: Value) -> Check<Value> {
        grant(
            db,
            &self.alpha,
            &self.alice,
            &self.bob,
            &self.peer,
            invocation,
        )
    }
}

#[test]
fn invocation_scope_idempotency_and_tenant_isolation() -> Check {
    let f = HubFixture::new()?;
    let mut db = ok(f.hub.lock())?;
    let p = Pair::new(&f, &mut db)?;
    let carol = account(&db, "carol")?;
    let viewer = account(&db, "viewer")?;
    join(&db, &p.alpha, &carol, "member")?;
    join(&db, &p.alpha, &viewer, "viewer")?;
    let a = p.alice();
    let b = p.bob();
    let mut implicit = proposal(p.section(), &p.peer, json!({"mode":"owner"}));
    implicit["policy"]
        .as_object_mut()
        .ok_or("invalid policy")?
        .remove("invocation");
    let first = ok(authority::propose(&db, &b, &implicit))?;
    ok(authority::consent(&db, &a, &first, None))?;
    ok(authority::consent(&db, &b, &first, Some(&p.peer.principal)))?;
    let input = named(p.section(), &first);
    ok(post(&mut db, &b, &input))?;
    for ctx in [&a, &context(&p.alpha, &carol)] {
        rejects(post(&mut db, ctx, &input), "authority_denied");
    }
    let specific = p.peer_grant(&db, json!({"mode":"members","accountIds":[carol]}))?;
    ok(post(
        &mut db,
        &context(&p.alpha, &carol),
        &named(p.section(), &specific),
    ))?;
    rejects(
        post(&mut db, &a, &named(p.section(), &specific)),
        "authority_denied",
    );
    let everyone = p.peer_grant(&db, json!({"mode":"all-members"}))?;
    let input = named(p.section(), &everyone);
    let first = ok(post(&mut db, &b, &input))?;
    let before = ok(routing::pending(&db, &p.peer.principal))?.len();
    assert_eq!(ok(post(&mut db, &b, &input))?, first);
    assert_eq!(ok(routing::pending(&db, &p.peer.principal))?.len(), before);
    let mut changed = input.clone();
    changed["text"] = json!("Changed");
    rejects(post(&mut db, &b, &changed), "idempotency_conflict");
    assert_ne!(
        ok(post(&mut db, &a, &input))?["messageId"],
        first["messageId"]
    );
    rejects(
        post(&mut db, &context(&p.alpha, &viewer), &input),
        "authority_denied",
    );
    let beta = context(&p.beta, &p.bob);
    let beta_section = text(&p.beta, "sectionId");
    let foreign = grant(
        &db,
        &p.beta,
        &p.bob,
        &p.bob,
        &p.peer,
        json!({"mode":"owner"}),
    )?;
    rejects(
        post(&mut db, &b, &named(p.section(), &foreign)),
        "resource_not_found",
    );
    let mut cross = message(beta_section, json!([]));
    cross["requestId"] = input["requestId"].clone();
    rejects(post(&mut db, &b, &cross), "resource_not_found");
    assert_ne!(
        ok(post(&mut db, &beta, &cross))?["messageId"],
        first["messageId"]
    );
    let turn = first["turnIds"][0].as_str().ok_or("missing turn")?;
    rejects(routing::turn(&db, &beta, turn), "resource_not_found");
    rejects(
        routing::history(&db, &beta, p.section()),
        "resource_not_found",
    );
    assert!(db
        .execute(
            "INSERT INTO participation_sections VALUES(?,?,?)",
            params![
                a.project,
                everyone["participationId"].as_str(),
                beta_section
            ]
        )
        .is_err());
    Ok(())
}

#[test]
fn section_event_and_attribution_stay_scoped_to_active_participants() -> Check {
    let f = HubFixture::new()?;
    let mut db = ok(f.hub.lock())?;
    let p = Pair::new(&f, &mut db)?;
    let a = p.alice();
    let foreign = node(&db, &p.eve)?;
    let alternate = node(&db, &p.alice)?;
    let other_section = new_id();
    db.execute(
        "INSERT INTO sections(id,project_id,name) VALUES(?,?,'other')",
        params![other_section, a.project],
    )?;
    let mut extra = p.alpha.clone();
    extra["sectionId"] = json!(other_section);
    let own = p.own_grant(&db)?;
    let peer = p.peer_grant(&db, json!({"mode":"all-members"}))?;
    grant(
        &db,
        &p.beta,
        &p.bob,
        &p.bob,
        &p.peer,
        json!({"mode":"all-members"}),
    )?;
    grant(
        &db,
        &extra,
        &p.alice,
        &p.alice,
        &alternate,
        json!({"mode":"owner"}),
    )?;
    ok(post(&mut db, &a, &named(p.section(), &peer)))?;
    for node in [&p.owner, &p.peer] {
        assert_eq!(
            ok(routing::pending(&db, &node.principal))?
                .iter()
                .filter(|frame| frame["kind"] == "section.event")
                .count(),
            1
        );
    }
    for node in [&foreign, &alternate] {
        assert!(ok(routing::pending(&db, &node.principal))?.is_empty());
    }
    let req = request(&db, &p.peer)?;
    assert_eq!(req["projectId"], a.project);
    assert_eq!(req["requesterAccountId"], p.alice);
    assert_eq!(req["payerAccountId"], p.bob);
    assert_eq!(req["payerBindingRef"], "node-local-payer");
    assert_ne!(req["participationId"], own["participationId"]);
    assert_eq!(ok(routing::grants(&db, &p.peer.principal))?.len(), 2);
    Ok(())
}

#[test]
fn ordered_results_preserve_receipts_and_reject_conflicts() -> Check {
    let f = HubFixture::new()?;
    let mut db = ok(f.hub.lock())?;
    let p = Pair::new(&f, &mut db)?;
    let a = p.alice();
    let proposal = p.peer_grant(&db, json!({"mode":"all-members"}))?;
    ok(post(&mut db, &a, &named(p.section(), &proposal)))?;
    let req = request(&db, &p.peer)?;
    let admitted = result(&req, "turn.admitted", 1, json!({}));
    let mut wrong = admitted.clone();
    wrong["nodeId"] = json!(p.owner.id);
    rejects(accept(&db, &p.owner, &wrong), "resource_not_found");
    wrong = admitted.clone();
    wrong["sectionId"] = p.beta["sectionId"].clone();
    rejects(accept(&db, &p.peer, &wrong), "authority_denied");
    rejects(
        accept(
            &db,
            &p.peer,
            &result(&req, "turn.final", 1, json!({"text":"Premature"})),
        ),
        "authority_denied",
    );
    assert_eq!(ok(accept(&db, &p.peer, &admitted))?["duplicate"], false);
    assert_eq!(ok(accept(&db, &p.peer, &admitted))?["duplicate"], true);
    wrong = admitted.clone();
    wrong["eventId"] = json!(new_id());
    rejects(accept(&db, &p.peer, &wrong), "idempotency_conflict");
    let partial = result(&req, "turn.partial", 2, json!({"text":"Answer"}));
    wrong = partial.clone();
    wrong["payloadDigest"] = json!(digest("wrong"));
    rejects(accept(&db, &p.peer, &wrong), "digest_mismatch");
    wrong = partial.clone();
    wrong["sequence"] = json!(3);
    rejects(accept(&db, &p.peer, &wrong), "stale_revision");
    assert_eq!(ok(accept(&db, &p.peer, &partial))?["sequence"], 2);
    let done = result(&req, "turn.final", 3, json!({"text":"Answer complete"}));
    ok(accept(&db, &p.peer, &done))?;
    ok(routing::pending(&db, &p.peer.principal))?;
    assert_eq!(ok(accept(&db, &p.peer, &done))?["duplicate"], true);
    let turn = text(&req, "turnId");
    assert!(db
        .execute(
            "UPDATE turns SET payer_account_id=? WHERE id=?",
            params![p.alice, turn]
        )
        .is_err());
    assert_eq!(ok(routing::turn(&db, &p.bob(), turn))?["state"], "done");
    assert_eq!(
        ok(routing::history(&db, &a, p.section()))?["results"]
            .as_array()
            .ok_or("missing results")?
            .len(),
        3
    );
    rejects(
        accept(
            &db,
            &p.peer,
            &result(&req, "turn.partial", 4, json!({"text":"Late"})),
        ),
        "stale_revision",
    );
    Ok(())
}

#[test]
fn action_lease_and_result_receipts_recheck_authority() -> Check {
    let f = HubFixture::new()?;
    let mut db = ok(f.hub.lock())?;
    let p = Pair::new(&f, &mut db)?;
    let a = p.alice();
    let proposal = p.own_grant(&db)?;
    ok(post(&mut db, &a, &named(p.section(), &proposal)))?;
    let req = request(&db, &p.owner)?;
    let input = json!({"projectId":req["projectId"],"turnId":req["turnId"],"actionId":new_id(),"actionDigest":digest("effect"),"fence":req["fence"]});
    rejects(
        routing::lease(&db, &p.owner.principal, &input),
        "authority_denied",
    );
    ok(accept(
        &db,
        &p.owner,
        &result(&req, "turn.admitted", 1, json!({})),
    ))?;
    let before = now();
    let lease = ok(routing::lease(&db, &p.owner.principal, &input))?;
    let expires = lease["expiresAt"].as_i64().ok_or("missing lease expiry")?;
    assert!((before + 5000..=now() + 5000).contains(&expires));
    assert_eq!(ok(routing::lease(&db, &p.owner.principal, &input))?, lease);
    let mut changed = input.clone();
    changed["actionDigest"] = json!(digest("changed"));
    rejects(
        routing::lease(&db, &p.owner.principal, &changed),
        "idempotency_conflict",
    );
    let receipt = json!({"actionId":input["actionId"],"actionDigest":input["actionDigest"],"leaseId":lease["leaseId"],"outcome":"succeeded"});
    let mut invalid = receipt.clone();
    invalid["leaseId"] = json!(new_id());
    rejects(
        accept(
            &db,
            &p.owner,
            &result(&req, "turn.effect.receipt", 2, invalid),
        ),
        "authority_denied",
    );
    ok(accept(
        &db,
        &p.owner,
        &result(&req, "turn.effect.receipt", 2, receipt),
    ))?;
    advance(5_000);
    rejects(
        routing::lease(&db, &p.owner.principal, &input),
        "stale_revision",
    );
    ok(authority::withdraw(
        &db,
        &a,
        &json!({"participationId":proposal["participationId"]}),
    ))?;
    changed["actionId"] = json!(new_id());
    rejects(
        routing::lease(&db, &p.owner.principal, &changed),
        "authority_denied",
    );
    Ok(())
}

#[test]
fn revocation_fences_queued_delivery_and_late_results_but_preserves_beta() -> Check {
    let f = HubFixture::new()?;
    let mut db = ok(f.hub.lock())?;
    let p = Pair::new(&f, &mut db)?;
    let a = p.alice();
    let shared = p.peer_grant(&db, json!({"mode":"all-members"}))?;
    let beta = grant(
        &db,
        &p.beta,
        &p.bob,
        &p.bob,
        &p.peer,
        json!({"mode":"owner"}),
    )?;
    ok(post(&mut db, &a, &named(p.section(), &shared)))?;
    let req = request(&db, &p.peer)?;
    ok(accept(
        &db,
        &p.peer,
        &result(&req, "turn.admitted", 1, json!({})),
    ))?;
    ok(authority::edit_member(
        &db,
        &a,
        &json!({"accountId":p.bob,"revision":1,"role":"revoked"}),
    ))?;
    assert_eq!(
        ok(routing::turn(&db, &a, text(&req, "turnId")))?["state"],
        "unknown"
    );
    rejects(
        accept(
            &db,
            &p.peer,
            &result(&req, "turn.final", 2, json!({"text":"Late"})),
        ),
        "resource_not_found",
    );
    rejects(
        post(&mut db, &a, &named(p.section(), &shared)),
        "resource_not_found",
    );
    let pending = ok(routing::pending(&db, &p.peer.principal))?;
    for kind in ["grant.revoked", "turn.cancel"] {
        assert!(pending.iter().any(|frame| frame["kind"] == kind));
    }
    assert!(pending
        .iter()
        .all(|frame| frame["kind"] != "turn.request" && frame["kind"] != "section.event"));
    let beta_ctx = context(&p.beta, &p.bob);
    assert_eq!(
        ok(post(
            &mut db,
            &beta_ctx,
            &named(text(&p.beta, "sectionId"), &beta)
        ))?["turnIds"]
            .as_array()
            .ok_or("missing turns")?
            .len(),
        1
    );
    db.execute(
        "UPDATE memberships SET status='active' WHERE project_id=? AND account_id=?",
        params![a.project, p.bob],
    )?;
    rejects(
        post(&mut db, &p.bob(), &named(p.section(), &shared)),
        "authority_denied",
    );
    Ok(())
}

#[test]
fn requester_removal_does_not_revoke_owner_consent() -> Check {
    let f = HubFixture::new()?;
    let mut db = ok(f.hub.lock())?;
    let p = Pair::new(&f, &mut db)?;
    let carol = account(&db, "carol")?;
    join(&db, &p.alpha, &carol, "member")?;
    let grant = p.peer_grant(&db, json!({"mode":"all-members"}))?;
    let requested = ok(post(
        &mut db,
        &context(&p.alpha, &carol),
        &named(p.section(), &grant),
    ))?;
    ok(authority::edit_member(
        &db,
        &p.alice(),
        &json!({"accountId":carol,"revision":1,"role":"revoked"}),
    ))?;
    let id = requested["turnIds"][0].as_str().ok_or("missing turn")?;
    assert_eq!(ok(routing::turn(&db, &p.alice(), id))?["state"], "revoked");
    assert_eq!(
        ok(post(&mut db, &p.bob(), &named(p.section(), &grant)))?["turnIds"]
            .as_array()
            .ok_or("missing turns")?
            .len(),
        1
    );
    Ok(())
}

#[test]
fn demotion_archived_section_revoked_node_and_blocked_account_fail_closed() -> Check {
    let f = HubFixture::new()?;
    let mut db = ok(f.hub.lock())?;
    let p = Pair::new(&f, &mut db)?;
    let shared = p.peer_grant(&db, json!({"mode":"owner"}))?;
    ok(authority::edit_member(
        &db,
        &p.alice(),
        &json!({"accountId":p.bob,"revision":1,"role":"viewer"}),
    ))?;
    rejects(
        post(&mut db, &p.bob(), &named(p.section(), &shared)),
        "authority_denied",
    );
    rejects(
        authority::propose(
            &db,
            &p.bob(),
            &proposal(p.section(), &p.peer, json!({"mode":"owner"})),
        ),
        "authority_denied",
    );
    ok(authority::withdraw(
        &db,
        &p.bob(),
        &json!({"participationId":shared["participationId"]}),
    ))?;
    let owned = p.own_grant(&db)?;
    ok(post(&mut db, &p.alice(), &named(p.section(), &owned)))?;
    db.execute("UPDATE sections SET archived=1 WHERE id=?", [p.section()])?;
    assert!(!ok(routing::pending(&db, &p.owner.principal))?
        .iter()
        .any(|frame| frame["kind"] == "turn.request"));
    ok(authority::revoke_node(&db, &p.owner.principal))?;
    rejects(
        routing::pending(&db, &p.owner.principal),
        "resource_not_found",
    );
    ok(authority::account_status(&db, &p.bob, "blocked"))?;
    let beta = context(&p.beta, &p.bob);
    rejects(authority::member(&db, &beta, &[]), "account_unavailable");
    ok(authority::account_status(&db, &p.bob, "active"))?;
    assert_eq!(ok(authority::member(&db, &beta, &[]))?["role"], "owner");
    rejects(routing::pending(&db, &p.peer.principal), "stale_revision");
    Ok(())
}

fn messages(db: &Connection) -> Check<i64> {
    Ok(db.query_row("SELECT count(*) FROM messages", [], |row| row.get(0))?)
}

#[test]
fn offline_admission_is_atomic_and_reconnect_fences_old_node_generation() -> Check {
    let f = HubFixture::new()?;
    let mut db = ok(f.hub.lock())?;
    let p = Pair::new(&f, &mut db)?;
    let grant = p.own_grant(&db)?;
    ok(post(&mut db, &p.alice(), &named(p.section(), &grant)))?;
    let req = request(&db, &p.owner)?;
    ok(accept(
        &db,
        &p.owner,
        &result(&req, "turn.admitted", 1, json!({})),
    ))?;
    storage::fence_restart(&mut db)?;
    let before = messages(&db)?;
    rejects(
        post(&mut db, &p.alice(), &named(p.section(), &grant)),
        "node_offline",
    );
    assert_eq!(messages(&db)?, before);
    assert_eq!(
        ok(routing::turn(&db, &p.alice(), text(&req, "turnId")))?["state"],
        "unknown"
    );
    db.execute(
        "UPDATE nodes SET status='ready',connection_generation=2 WHERE id=?",
        [&p.owner.id],
    )?;
    rejects(routing::pending(&db, &p.owner.principal), "stale_revision");
    let mut fresh = p.owner.principal.clone();
    fresh["connectionGeneration"] = json!(2);
    assert!(ok(routing::pending(&db, &fresh))?.is_empty());
    rejects(
        routing::accept(
            &db,
            &fresh,
            &result(&req, "turn.final", 2, json!({"text":"Old connection"})),
        ),
        "stale_revision",
    );
    assert_eq!(ok(routing::grants(&db, &fresh))?.len(), 1);
    Ok(())
}

#[test]
fn outbox_ack_binds_event_digest_and_current_consent() -> Check {
    let f = HubFixture::new()?;
    let mut db = ok(f.hub.lock())?;
    let p = Pair::new(&f, &mut db)?;
    let grant = p.own_grant(&db)?;
    let input = named(p.section(), &grant);
    ok(post(&mut db, &p.alice(), &input))?;
    let req = request(&db, &p.owner)?;
    let event = text(&req, "eventId");
    let mut ack = json!({"eventId":event,"frameDigest":digest("wrong")});
    rejects(
        routing::ack(&db, &p.owner.principal, &ack),
        "idempotency_conflict",
    );
    ack["frameDigest"] = json!(digest(&canonical(&req)));
    ok(routing::ack(&db, &p.owner.principal, &ack))?;
    assert!(!ok(routing::pending(&db, &p.owner.principal))?
        .iter()
        .any(|frame| frame["eventId"] == event));
    ok(authority::withdraw(
        &db,
        &p.alice(),
        &json!({"participationId":grant["participationId"]}),
    ))?;
    rejects(post(&mut db, &p.alice(), &input), "authority_denied");
    rejects(
        routing::ack(&db, &p.owner.principal, &ack),
        "resource_not_found",
    );
    Ok(())
}

#[test]
fn malformed_policy_frames_and_oversized_outbound_roll_back() -> Check {
    let f = HubFixture::new()?;
    let mut db = ok(f.hub.lock())?;
    let p = Pair::new(&f, &mut db)?;
    let a = p.alice();
    let grant = p.own_grant(&db)?;
    let policy = proposal(p.section(), &p.owner, json!({"mode":"owner"}));
    let mut invalid = policy.clone();
    invalid["policy"]["sectionIds"] = json!([p.beta["sectionId"]]);
    rejects(authority::propose(&db, &a, &invalid), "resource_not_found");
    invalid = policy.clone();
    invalid["policy"]["invocation"] = json!({"mode":"members","accountIds":[p.eve]});
    rejects(authority::propose(&db, &a, &invalid), "resource_not_found");
    invalid = policy.clone();
    invalid["policy"]["payerBindingRef"] = json!("/home/secret");
    rejects(authority::propose(&db, &a, &invalid), "invalid_policy");
    let mut oversized = named(p.section(), &grant);
    oversized["text"] = json!("\u{0001}".repeat(16000));
    rejects(post(&mut db, &a, &oversized), "payload_too_large");
    assert_eq!(messages(&db)?, 0);
    assert_eq!(
        db.query_row("SELECT count(*) FROM turns", [], |r| r.get::<_, i64>(0))?,
        0
    );
    ok(post(&mut db, &a, &named(p.section(), &grant)))?;
    let req = request(&db, &p.owner)?;
    let admitted = result(&req, "turn.admitted", 1, json!({}));
    let mut unknown = admitted.clone();
    unknown["providerKey"] = json!("unexpected");
    rejects(accept(&db, &p.owner, &unknown), "unknown_field");
    for field in req["fence"].as_object().ok_or("missing fence")?.keys() {
        let mut forged = admitted.clone();
        forged["fence"][field] = if field == "authorityGeneration" {
            json!(new_id())
        } else {
            json!(99)
        };
        rejects(accept(&db, &p.owner, &forged), "authority_denied");
    }
    ok(accept(&db, &p.owner, &admitted))?;
    rejects(
        accept(
            &db,
            &p.owner,
            &result(&req, "turn.partial", 2, json!({"text":"x".repeat(8001)})),
        ),
        "invalid_text",
    );
    let approval = json!({"actionId":new_id(),"actionDigest":digest("effect"),"requiredApproverAccountId":p.bob,"expiresAt":now()+1000});
    rejects(
        accept(
            &db,
            &p.owner,
            &result(&req, "turn.approval.request", 2, approval),
        ),
        "authority_denied",
    );
    Ok(())
}

#[test]
fn expiry_and_publication_bounds_preserve_exact_approval_replay() -> Check {
    let f = HubFixture::new()?;
    let mut db = ok(f.hub.lock())?;
    let p = Pair::new(&f, &mut db)?;
    let policy = p.own_grant(&db)?;
    let ctx = p.alice();
    ok(post(&mut db, &ctx, &named(p.section(), &policy)))?;
    let req = request(&db, &p.owner)?;
    ok(accept(
        &db,
        &p.owner,
        &result(&req, "turn.admitted", 1, json!({})),
    ))?;
    let approval = json!({"actionId":new_id(),"actionDigest":digest("effect"),"requiredApproverAccountId":p.alice,"expiresAt":now()+1000});
    let approval = result(&req, "turn.approval.request", 2, approval);
    ok(accept(&db, &p.owner, &approval))?;
    advance(1001);
    assert_eq!(ok(accept(&db, &p.owner, &approval))?["duplicate"], true);
    let invented = result(
        &req,
        "turn.approval.request",
        3,
        approval["payload"].clone(),
    );
    rejects(accept(&db, &p.owner, &invented), "authority_denied");
    let rows: i64 = db.query_row("SELECT count(*) FROM node_results", [], |r| r.get(0))?;
    assert_eq!(rows, 2);
    for sequence in 3..=34 {
        let part = result(
            &req,
            "turn.partial",
            sequence,
            json!({"text":"x".repeat(8000)}),
        );
        ok(accept(&db, &p.owner, &part))?;
    }
    rejects(
        accept(
            &db,
            &p.owner,
            &result(&req, "turn.partial", 35, json!({"text":"x".repeat(8000)})),
        ),
        "result_limit",
    );
    advance(600_000);
    rejects(
        accept(
            &db,
            &p.owner,
            &result(&req, "turn.final", 35, json!({"text":"Late"})),
        ),
        "stale_revision",
    );
    assert!(ok(routing::pending(&db, &p.owner.principal))?.is_empty());
    let lease = json!({"projectId":req["projectId"],"turnId":req["turnId"],"actionId":new_id(),"actionDigest":digest("x"),"fence":req["fence"]});
    rejects(
        routing::lease(&db, &p.owner.principal, &lease),
        "stale_revision",
    );
    Ok(())
}

#[test]
fn hub_database_markers_migrations_and_writer_aliases() -> Check {
    use minarbot_launcher::data_format;
    let root = Temp::new()?;
    let path = root.path("fresh.sqlite");
    let (db, writer) = storage::open(&path)?;
    assert!(data_format::is_marked(&db)?);
    assert_eq!(
        db.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))?,
        "wal"
    );
    assert_eq!(
        db.pragma_query_value(None, "foreign_keys", |row| row.get::<_, i64>(0))?,
        1
    );
    assert!(storage::open(&path).is_err_and(|error| error.to_string().contains("exists")));
    let symlink = root.path("symbolic.sqlite");
    std::os::unix::fs::symlink(&path, &symlink)?;
    assert!(storage::open(&symlink).is_err_and(|error| error.to_string().contains("aliases")));
    let hard_link = root.path("hard.sqlite");
    fs::hard_link(&path, &hard_link)?;
    assert!(storage::open(&hard_link).is_err_and(|error| error.to_string().contains("aliases")));
    fs::remove_file(hard_link)?;
    let migration = db.query_row(
        "SELECT checksum FROM hub_migrations WHERE version=1",
        [],
        |row| row.get::<_, String>(0),
    )?;
    drop((db, writer));
    {
        let (db, _writer) = storage::open(&path)?;
        assert_eq!(
            db.query_row(
                "SELECT checksum FROM hub_migrations WHERE version=1",
                [],
                |row| row.get::<_, String>(0)
            )?,
            migration
        );
        db.execute("UPDATE hub_migrations SET checksum='tampered'", [])?;
    }
    assert!(storage::open(&path).is_err_and(|error| error.to_string().contains("checksum")));
    assert_eq!(
        Connection::open(&path)?.query_row("SELECT count(*) FROM hub_state", [], |row| row
            .get::<_, i64>(0))?,
        1
    );

    let v0 = root.path("migration-v0.sqlite");
    {
        let db = Connection::open(&v0)?;
        fs::set_permissions(&v0, fs::Permissions::from_mode(0o600))?;
        data_format::mark(&db)?;
        db.execute_batch("CREATE TABLE hub_migrations(version INTEGER PRIMARY KEY, checksum TEXT NOT NULL) STRICT;")?;
    }
    let (db, writer) = storage::open(&v0)?;
    assert_eq!(
        db.query_row(
            "SELECT checksum FROM hub_migrations WHERE version=1",
            [],
            |row| row.get::<_, String>(0)
        )?,
        migration
    );
    drop((db, writer));
    let (db, _writer) = storage::open(&v0)?;
    assert_eq!(
        db.query_row("SELECT count(*) FROM hub_state", [], |row| row
            .get::<_, i64>(0))?,
        1
    );

    for (name, sql, marked, reason) in [
        ("unmarked", "CREATE TABLE hub_migrations(version INTEGER PRIMARY KEY, checksum TEXT NOT NULL) STRICT;", false, "data marker"),
        ("future", "CREATE TABLE hub_migrations(version INTEGER PRIMARY KEY, checksum TEXT NOT NULL) STRICT; INSERT INTO hub_migrations VALUES(99,'other');", true, "version"),
        ("foreign", "CREATE TABLE agents(id TEXT);", true, "non-hub"),
        ("partial", "CREATE TABLE hub_migrations(version INTEGER PRIMARY KEY, checksum TEXT NOT NULL) STRICT; CREATE TABLE sections(id TEXT);", true, "version"),
    ] {
        let file = root.path(name);
        let db = Connection::open(&file)?;
        fs::set_permissions(&file,fs::Permissions::from_mode(0o600))?;
        db.execute_batch(sql)?;
        if marked {
            data_format::mark(&db)?;
        }
        assert!(storage::open(&file).is_err_and(|error|error.to_string().contains(reason)));
        assert_eq!(data_format::is_marked(&db)?,marked);
        assert!(!db.prepare("SELECT 1 FROM sqlite_schema WHERE name='hub_state' OR name='accounts'")?.exists([])?);
        if name=="partial" {
            assert_eq!(db.query_row("SELECT count(*) FROM hub_migrations",[],|row|row.get::<_,i64>(0))?,0);
        }
    }
    Ok(())
}

#[test]
fn backup_reopen_and_restore_fence_pending_invites_turns_and_stale_principals() -> Check {
    let root = Temp::new()?;
    let backup = root.path("backup.sqlite");
    let f = HubFixture::new()?;
    let path = f.hub.config.data_path.clone();
    let mut db = ok(f.hub.lock())?;
    let p = Pair::new(&f, &mut db)?;
    let a = p.alice();
    let invitation = invite(&f.hub, &db, &a, "eve@example.test", "viewer")?;
    let policy = p.own_grant(&db)?;
    let mut input = named(p.section(), &policy);
    input["text"] = json!("Persisted");
    let posted = ok(post(&mut db, &a, &input))?;
    let before = ok(generation(&db))?;
    let mut copy = Connection::open(&backup)?;
    fs::set_permissions(&backup, fs::Permissions::from_mode(0o600))?;
    rusqlite::backup::Backup::new(&db, &mut copy)?.run_to_completion(
        128,
        Duration::from_millis(1),
        None,
    )?;
    drop((db, copy));
    assert_eq!(fs::metadata(&backup)?.permissions().mode() & 0o777, 0o600);
    assert!(storage::command(&backup, &["backup".into(), backup.as_os_str().to_owned()]).is_err());
    let archived = root.path("frozen.sqlite");
    storage::command(&backup, &["backup".into(), archived.as_os_str().to_owned()])?;
    {
        let (restored, _writer) = storage::open(&archived)?;
        assert_eq!(
            restored.query_row("SELECT text FROM messages", [], |r| r.get::<_, String>(0))?,
            "Persisted"
        );
    }
    storage::command(&archived, &["fence-restore".into()])?;
    let generation = {
        let (restored, _writer) = storage::open(&archived)?;
        rejects(
            authority::member(&restored, &a, &[]),
            "authority_unavailable",
        );
        restored.query_row("SELECT authority_generation FROM hub_state", [], |r| {
            r.get::<_, String>(0)
        })?
    };
    assert_ne!(generation, before);
    assert!(storage::command(&archived, &["activate".into(), before.into()]).is_err());
    storage::command(&archived, &["activate".into(), generation.into()])?;
    let (restored, _writer) = storage::open(&archived)?;
    assert_eq!(
        ok(routing::turn(
            &restored,
            &a,
            posted["turnIds"][0].as_str().ok_or("missing turn")?
        ))?["state"],
        "unknown"
    );
    assert_eq!(
        restored.query_row("SELECT count(*) FROM outbox", [], |r| r.get::<_, i64>(0))?,
        0
    );
    rejects(
        routing::pending(&restored, &p.owner.principal),
        "stale_revision",
    );
    rejects(
        authority::accept(
            &f.hub,
            &restored,
            &context(&p.alpha, &p.eve),
            &invitation,
            &identity("eve", "eve@example.test")?,
        ),
        "invitation_unavailable",
    );
    assert!(path.is_file());
    Ok(())
}

#[test]
fn result_admission_rejects_withdrawn_participation() -> Check {
    let f = HubFixture::new()?;
    let mut db = ok(f.hub.lock())?;
    let p = Pair::new(&f, &mut db)?;
    let grant = p.peer_grant(&db, json!({"mode":"all-members"}))?;
    ok(post(&mut db, &p.alice(), &named(p.section(), &grant)))?;
    let req = request(&db, &p.peer)?;
    db.execute(
        "UPDATE participations SET state='revoked' WHERE project_id=? AND id=?",
        params![req["projectId"].as_str(), req["participationId"].as_str()],
    )?;
    rejects(
        accept(&db, &p.peer, &result(&req, "turn.admitted", 1, json!({}))),
        "stale_revision",
    );
    Ok(())
}
