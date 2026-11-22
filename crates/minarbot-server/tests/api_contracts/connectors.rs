use super::{id, json};
use crate::server::{Result, TestRoot};
use reqwest::header;
use rusqlite::{params, Connection};
use serde_json::json;

#[tokio::test]
async fn calendar_preserves_folded_escaped_text_and_exact_utc_times() -> Result<()> {
    let root = TestRoot::new()?;
    let now = "2026-01-01T00:00:00Z"
        .parse::<chrono::DateTime<chrono::Utc>>()?
        .timestamp_millis();
    let app = root.start(json!({"fixedClock":now})).await?;
    let agent = id();
    json(app.post("/api/agents").json(&json!({"id":agent,"runtime":"api","name":"Calendar","model":"fixture","instructions":"Help"})), 201).await?;
    let prefix = "Calendar; comma, \\ line\r\nBEGIN:VEVENT ";
    let long = format!("{prefix}{}", "é".repeat(100));
    let mut entries = Vec::new();
    let db = Connection::open(root.database())?;
    let mut insert = db.prepare("INSERT INTO routines(id,agent,name,next_at,interval_ms,revision,schedule_json,schedule_zone,prompt,enabled,trigger) VALUES(?,?,?,?,?,?,?,?,'Fixture',1,'schedule')")?;
    let mut add = |name: &str,
                   offset,
                   interval: Option<i64>,
                   revision,
                   schedule: Option<String>,
                   zone: Option<&str>|
     -> Result<()> {
        let routine = id();
        insert.execute(params![
            routine,
            agent,
            name,
            now + offset,
            interval,
            revision,
            schedule,
            zone
        ])?;
        entries.push((routine, revision, name.to_owned()));
        Ok(())
    };
    add(&long, 0, Some(3600000), 1, None, None)?;
    for (index, (zone, date, time)) in [
        ("America/New_York", "2026-03-08", "02:30"),
        ("America/New_York", "2026-11-01", "01:30"),
        ("Pacific/Apia", "2011-12-30", "12:00"),
        ("Invalid/Zone", "2026-01-01", "12:00"),
    ]
    .into_iter()
    .enumerate()
    {
        add(
            zone,
            i64::try_from(index)? + 1,
            None,
            2,
            Some(json!({"kind":"once","date":date,"time":time}).to_string()),
            Some(zone),
        )?;
    }
    add(
        "Cron",
        5,
        None,
        3,
        Some(json!({"kind":"cron","expression":"0 0 * * *"}).to_string()),
        None,
    )?;
    add("Long interval", 6, Some(2147483647001), 4, None, None)?;
    let path = format!("/api/agents/{agent}/routines/calendar.ics");
    let calendar = app.get(&path).send().await?;
    assert_eq!(calendar.status(), 200);
    assert_eq!(
        calendar.headers()[header::CONTENT_TYPE],
        "text/calendar; charset=utf-8"
    );
    let raw = calendar.text().await?;
    assert!(raw.ends_with("END:VCALENDAR\r\n"));
    assert!(raw.split("\r\n").all(|line| line.len() <= 75));
    let expected_header = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Minarbot//Routines//EN\r\nCALSCALE:GREGORIAN\r\n";
    let mut expected = expected_header.to_owned();
    for (index, ((routine, revision, name), start)) in entries
        .iter()
        .zip([
            "20260101T000000Z",
            "20260308T070000Z",
            "20261101T053000Z",
            "20111230T100000Z",
        ])
        .enumerate()
    {
        let summary = if index == 0 {
            format!(
                "Calendar\\; comma\\, \\\\ line\\nBEGIN:VEVENT {}",
                "é".repeat(100)
            )
        } else {
            name.clone()
        };
        expected.push_str(&format!("BEGIN:VEVENT\r\nUID:{routine}@minarbot.local\r\nDTSTAMP:20260101T000000Z\r\nSEQUENCE:{revision}\r\nDTSTART:{start}\r\nSUMMARY:{summary}\r\n"));
        if index == 0 {
            expected.push_str("RRULE:FREQ=SECONDLY;INTERVAL=3600\r\n");
        }
        expected.push_str("END:VEVENT\r\n");
    }
    expected.push_str(concat!(
        "X-WR-CALDESC:Omitted routine \"Invalid/Zone\": home device time zone cannot resolve its schedule. ",
        "Omitted cron routine \"Cron\": cron recurrence cannot be represented safely. ",
        "Omitted routine \"Long interval\": interval exceeds the calendar limit.\r\nEND:VCALENDAR\r\n"
    ));
    assert_eq!(raw.replace("\r\n ", ""), expected);
    assert_eq!(
        app.get(&path).bearer_auth("fixture").send().await?.status(),
        403
    );
    Ok(())
}

#[tokio::test]
async fn mail_retries_exact_envelope_and_fences_unusable_jobs() -> Result<()> {
    use super::{
        auth::{canonical_config, CLOCK},
        until,
    };
    use crate::loopback::Loopback;
    use axum::{
        body::{Body, Bytes},
        response::{IntoResponse, Response},
        routing::post,
        Json, Router,
    };
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
    use ring::{
        aead,
        rand::{SecureRandom, SystemRandom},
    };
    use rustix::process::Signal;
    use sha2::{Digest, Sha256};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    };

    let root = TestRoot::new()?;
    let mut options = canonical_config();
    let app = root.start(options.clone()).await?;
    assert!(app.terminate(Signal::TERM).await?.success());
    let db = Connection::open(root.database())?;
    let key = [7; 32];
    let key_id = id();
    let owner = id();
    let project = id();
    let generation: String = db.query_row("SELECT generation FROM authority_state", [], |row| {
        row.get(0)
    })?;
    db.execute(
        "INSERT INTO accounts(id,issuer,clerk_sub,created) VALUES(?,'fixture','owner',?)",
        params![owner, CLOCK],
    )?;
    db.execute(
        "INSERT INTO projects VALUES(?,?,'Fixture',0,0,1)",
        params![project, owner],
    )?;
    db.execute(
        "INSERT INTO project_members VALUES(?,?,'owner',1,0)",
        params![project, owner],
    )?;
    let pending = |expires, authority: &str| -> Result<String> {
        let invitation = id();
        let email_hash = format!("{:x}", Sha256::digest(invitation.as_bytes()));
        let aad = serde_json::to_vec(&json!([
            "minarbot-mail-outbox-v1",
            invitation,
            1,
            authority,
            email_hash
        ]))?;
        let mut payload = serde_json::to_vec(
            &json!({"to":"member@example.test","from":"noreply@example.test","subject":"Invite","text":"Fixture link"}),
        )?;
        let digest = format!("{:x}", Sha256::digest(&payload));
        let mut nonce = [0; 12];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| "fixture nonce failed")?;
        let secret =
            aead::UnboundKey::new(&aead::AES_256_GCM, &key).map_err(|_| "invalid fixture key")?;
        let tag = aead::LessSafeKey::new(secret)
            .seal_in_place_separate_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(aad),
                &mut payload,
            )
            .map_err(|_| "fixture envelope failed")?;
        let envelope = serde_json::to_vec(&json!({
            "v":1,"alg":"AES-256-GCM","keyId":key_id,"nonce":B64.encode(nonce),
            "ciphertext":B64.encode(payload),"tag":B64.encode(tag)
        }))?;
        let token_hash = format!("{:x}", Sha256::digest(id().as_bytes()));
        db.execute(
            "INSERT INTO invitations VALUES(?,?,?,?,NULL,'pending_delivery',?,?,?,NULL,1)",
            params![
                invitation,
                project,
                authority,
                email_hash,
                token_hash,
                r#"{"v":1,"projectRevision":1,"agents":[]}"#,
                expires
            ],
        )?;
        db.execute(
            "INSERT INTO mail_outbox VALUES(?,1,?,'queued',NULL,NULL,0,NULL,?)",
            params![invitation, envelope, digest],
        )?;
        Ok(invitation)
    };
    let expiry = CLOCK + 86400000;
    let first = pending(expiry, &generation)?;
    let expired = pending(CLOCK - 1, &generation)?;
    let revoked = pending(expiry, &id())?;
    let exhausted = pending(expiry, &generation)?;
    let tampered = pending(expiry, &generation)?;
    db.execute(
        "UPDATE mail_outbox SET attempts=8,next_attempt=? WHERE id=?",
        params![CLOCK + 60000, exhausted],
    )?;
    db.execute(
        "UPDATE mail_outbox SET payload_digest=? WHERE id=?",
        params!["0".repeat(64), tampered],
    )?;

    let requests = Arc::new(Mutex::new(Vec::new()));
    let lose = Arc::new(AtomicBool::new(true));
    let upstream = Loopback::start(Router::new().route(
        "/emails",
        post({
            let requests = requests.clone();
            let lose = lose.clone();
            move |headers: axum::http::HeaderMap, body: Bytes| {
                let requests = requests.clone();
                let lose = lose.clone();
                async move {
                    let token = headers.get("idempotency-key").cloned();
                    let authorized = headers
                        .get(header::AUTHORIZATION)
                        .is_some_and(|value| value == "Bearer synthetic-mail-key");
                    if let Ok(mut requests) = requests.lock() {
                        requests.push((body, token, authorized));
                    }
                    if lose.load(Ordering::SeqCst) {
                        let fault = std::io::Error::new(
                            std::io::ErrorKind::ConnectionReset,
                            "fixture lost response",
                        );
                        return Response::new(Body::from_stream(futures_util::stream::once(
                            async { Err::<Bytes, _>(fault) },
                        )));
                    }
                    Json(json!({"id":"fixture-mail"})).into_response()
                }
            }
        }),
    ))
    .await?;
    options["mailApiKey"] = json!("synthetic-mail-key");
    options["mailEndpoint"] = json!(format!("http://{}/emails", upstream.address));
    options["testKeyring"] = json!({
        format!("mail-aead/{key_id}"):B64.encode(key),
        "sync-hmac/v1":B64.encode([9;32]),
    });
    let state = |id: &str| -> Result<(String, i64, i64)> {
        Ok(db.query_row(
            "SELECT state,attempts,length(ciphertext) FROM mail_outbox WHERE id=?",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?)
    };
    let app = root.start(options.clone()).await?;
    until(async || Ok((state(&first)?.0 == "unknown").then_some(()))).await?;
    assert_eq!(
        requests
            .lock()
            .map_err(|_| "mail request log poisoned")?
            .len(),
        1
    );
    db.execute(
        "UPDATE mail_outbox SET next_attempt=0 WHERE id=?",
        [&exhausted],
    )?;
    until(async || Ok((state(&exhausted)?.0 == "unknown").then_some(()))).await?;
    let invitation_state: String = db.query_row(
        "SELECT state FROM invitations WHERE id=?",
        [&exhausted],
        |row| row.get(0),
    )?;
    assert_eq!(invitation_state, "delivery_unknown");
    assert!(app.terminate(Signal::TERM).await?.success());

    lose.store(false, Ordering::SeqCst);
    db.execute("UPDATE mail_outbox SET next_attempt=0 WHERE id=?", [&first])?;
    let _app = root.start(options).await?;
    until(async || Ok((state(&first)?.0 == "sent").then_some(()))).await?;
    let requests = requests.lock().map_err(|_| "mail request log poisoned")?;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].0, requests[1].0);
    assert!(requests[0].1.is_some());
    assert_eq!(requests[0].1, requests[1].1);
    assert!(requests.iter().all(|request| request.2));
    assert_eq!(state(&first)?.2, 0);
    assert_eq!(state(&expired)?.0, "revoked");
    assert_eq!(state(&revoked)?.0, "revoked");
    assert_eq!(state(&exhausted)?.2, 0);
    assert_eq!(state(&tampered)?.1, 0);
    Ok(())
}
