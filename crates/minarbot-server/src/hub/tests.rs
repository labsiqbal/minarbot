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
