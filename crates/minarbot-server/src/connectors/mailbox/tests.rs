use super::*;
use axum::body::Body;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

const NOW: i64 = 1_800_000_000_000;
const RAW: &[u8] = b"From: Verify <verify@example.test>\r\nTo: someone-else@example.test\r\nSubject: =?UTF-8?Q?V=C3=A9rification?=\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\nWW91ciBjb2RlIGlzOiAwMDEyOTQ=";

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn state() -> crate::db::Result<(Scratch, AppState)> {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("minarbot-mailbox-{}", uuid::Uuid::new_v4())));
    let config: crate::config::Config =
        serde_json::from_value(json!({"database":scratch.0.join("mail.sqlite"),"fixedClock":NOW}))?;
    let database = crate::db::open(&config.database, false, NOW)?;
    crate::files::init(&database.connection)?;
    crate::data::initialize(&database)?;
    crate::turns::initialize(&database.connection).map_err(|e| e.message)?;
    crate::computer::initialize(&database.connection)?;
    let events = crate::events::Hub::new();
    events.watch(&database.connection);
    let state = AppState {
        database: Arc::new(Mutex::new(database)),
        computer: Arc::new(crate::computer::Manager::new(&config)?),
        config: Arc::new(config),
        origin: "http://127.0.0.1".into(),
        auth: None,
        events,
        turns: crate::turns::Runtime::default(),
        trial_pending: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    Ok((scratch, state))
}

fn mailbox(state: &AppState) -> Result<(String, String, String, String)> {
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    let first = uuid::Uuid::new_v4().to_string();
    let second = uuid::Uuid::new_v4().to_string();
    for id in [&first, &second] {
        db.execute(
            "INSERT INTO agents(id,name,model,instructions) VALUES(?,'Same name','','')",
            [id],
        )?;
    }
    initialize(db)?;
    let saved = save_settings(state, db, &json!({"revision":0,"domain":"Example.Test"}))?;
    assert_eq!(saved["domain"], "example.test");
    let issued = save_settings(state, db, &json!({"revision":1,"rotateSecret":true}))?;
    let secret = string(&issued, "secret").to_owned();
    assert!(
        secret.len() == 43
            && !public_settings(&settings(db)?)
                .to_string()
                .contains(&secret)
    );
    let to = configured_address(db, &first)?;
    assert_ne!(to, configured_address(db, &second)?);
    Ok((first, second, to, secret))
}

fn request(to: &str, secret: &str, time: i64, raw: &'static [u8]) -> Result<Request> {
    let time = time.to_string();
    Request::builder()
        .method(Method::POST)
        .uri(INBOUND)
        .header("content-type", "message/rfc822")
        .header("x-minarbot-email-to", to)
        .header("x-minarbot-email-timestamp", &time)
        .header(
            "x-minarbot-email-signature",
            B64.encode(signature(secret.as_bytes(), &time, to, raw)),
        )
        .body(Body::from(raw))
        .map_err(|_| Failure::storage())
}

#[tokio::test]
async fn signed_rfc5322_delivery_idempotently_routes_the_smtp_recipient() -> crate::db::Result<()> {
    let (_scratch, state) = state()?;
    async {
        let (first, second, to, secret) = mailbox(&state)?;
        let timestamp = NOW / 1000;
        assert_eq!(inbound(&state, request("unknown@example.test", &secret, timestamp, RAW)?).await.err().map(|e|e.status), Some(404));
        assert_eq!(inbound(&state, request(&to, "bad", timestamp, RAW)?).await.err().map(|e|e.status), Some(401));
        assert_eq!(inbound(&state, request(&to, &secret, timestamp - 301, RAW)?).await.err().map(|e|e.status), Some(401));
        let mut changed = request(&to, &secret, timestamp, RAW)?;
        *changed.body_mut() = Body::from("changed body");
        assert_eq!(inbound(&state, changed).await.err().map(|e|e.status), Some(401));
        for _ in 0..2 {
            assert_eq!(inbound(&state, request(&to, &secret, timestamp, RAW)?).await?.status(), 204);
        }
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        let inbox = list(&state, db, &first, None)?;
        let items = inbox["items"].as_array().ok_or_else(Failure::storage)?;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["subject"], "Vérification");
        assert_eq!(items[0]["from"], "verify@example.test");
        let id = string(&items[0], "id");
        let email = read(&state, db, &first, id)?;
        assert!(string(&email,"text").trim() == "Your code is: 001294");
        assert_eq!(email["to"], to);
        assert_eq!(list(&state, db, &second, None)?["items"], json!([]));
        assert_eq!(read(&state, db, &second, id).err().map(|e|e.status), Some(404));
        db.execute("UPDATE agents SET name='Renamed' WHERE id=?", [&first])?;
        assert_eq!(configured_address(db, &first)?, to);
        let ciphertext: String = db.query_row("SELECT ciphertext FROM agent_email_messages WHERE id=?", [id], |r|r.get(0))?;
        assert!(!ciphertext.contains("verify@example.test"));
        let envelope = parse(b"From: fixture@example.test\r\nSubject: HTML mail\r\nContent-Type: text/html\r\n\r\n<p>Receipt copy</p>", &to)?;
        assert!(string(&envelope,"text").contains("Receipt copy"));
        Ok::<_, Failure>(())
    }.await.map_err(|e| e.message.into())
}

#[tokio::test(start_paused = true)]
async fn otp_wait_checks_sender_subject_and_recency_then_times_out() -> crate::db::Result<()> {
    let (_scratch, state) = state()?;
    async {
        let (first, second, to, secret) = mailbox(&state)?;
        let turn = {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            let db = &guard.connection;
            let id = uuid::Uuid::new_v4().to_string();
            db.execute("INSERT INTO turns(id,agent,text,status,created) VALUES(?,?,'Check the mail','running',?)",
                params![id, first, crate::data::iso(db, NOW)?])?;
            let config = settings(db)?;
            let key = key(&state, false)?;
            let stale = b"From: verify@example.test\r\nSubject: V\xc3\xa9rification\r\n\r\nYour code is 987654.";
            let timestamp = (NOW / 1000).to_string();
            let tag = signature(secret.as_bytes(),&timestamp,&to,stale);
            deliver(db, &key, &config, Delivery { timestamp: &timestamp, recipient: &to, signature: tag.as_ref(), raw: stale }, NOW - 1)?;
            one(db, "SELECT * FROM turns WHERE id=?", [id])?.ok_or_else(Failure::storage)?
        };
        let args = json!({"from":"VERIFY@example.test","subject":"VÉRIFICATION","timeoutSeconds":1});
        let timeout = tools::wait(&state, &turn, &args).await?;
        assert_eq!(timeout["status"], "timeout");
        let wait = tokio::spawn({
            let (state, turn, args) = (state.clone(), turn.clone(), args.clone());
            async move { tools::wait(&state, &turn, &args).await }
        });
        tokio::task::yield_now().await;
        let other = {
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            configured_address(&guard.connection, &second)?
        };
        inbound(&state, request(&other, &secret, NOW / 1000, RAW)?).await?;
        inbound(&state, request(&to, &secret, NOW / 1000, b"From: other@example.test\r\nSubject: =?UTF-8?Q?V=C3=A9rification?=\r\n\r\nCode: 204911")?).await?;
        inbound(&state, request(&to, &secret, NOW / 1000, b"From: verify@example.test\r\nSubject: Different subject\r\n\r\nCode: 280547")?).await?;
        tokio::task::yield_now().await;
        assert!(!wait.is_finished());
        inbound(&state, request(&to, &secret, NOW / 1000, RAW)?).await?;
        let result = wait.await.map_err(|_| Failure::storage())??;
        assert_eq!(result["status"], "received");
        assert!(result["code"] == "001294");
        Ok::<_, Failure>(())
    }.await.map_err(|e| e.message.into())
}
