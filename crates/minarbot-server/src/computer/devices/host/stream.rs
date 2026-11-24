use super::*;

pub(super) struct Frames {
    response: reqwest::Response,
    buffer: Vec<u8>,
}
impl Frames {
    pub(super) async fn next(&mut self) -> Result<Value> {
        loop {
            if let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
                if newline > LIMIT {
                    return Err(fail(
                        413,
                        "payload_too_large",
                        "Device frame exceeds 64 KiB.",
                    ));
                }
                let line: Vec<u8> = self.buffer.drain(..=newline).collect();
                return crate::web::parse_json(&line);
            }
            if self.buffer.len() > LIMIT {
                return Err(fail(
                    413,
                    "payload_too_large",
                    "Device frame exceeds 64 KiB.",
                ));
            }
            let bytes = self
                .response
                .chunk()
                .await
                .map_err(|_| transport())?
                .ok_or_else(transport)?;
            self.buffer.extend_from_slice(&bytes);
        }
    }
}

#[derive(Clone)]
pub(super) struct Session {
    pub(super) token: String,
    pub(super) connection: i64,
    reports_providers: bool,
}

impl Host {
    pub(super) async fn open_stream(&mut self) -> Result<(Session, Frames)> {
        let descriptor = self
            .identity
            .descriptor
            .as_ref()
            .ok_or_else(|| invalid("Device not enrolled."))?;
        let path = format!("/api/devices/{}/session", self.identity.device_id);
        let request = request_id();
        let body = json!({"requestId":request,"expectedGeneration":descriptor["deviceGeneration"]});
        let session =
            submit(self.signed(&path, Method::POST, body.to_string(), &request, None)?).await?;
        let data = &session["data"];
        decode(&data["deviceSession"], 32)?;
        let token = text(data, "deviceSession").to_owned();
        if data["generation"] != descriptor["deviceGeneration"] {
            return Err(invalid("Device session generation does not match."));
        }
        let path = format!("/api/devices/{}/stream", self.identity.device_id);
        let response = self
            .signed(
                &path,
                Method::GET,
                String::new(),
                &request_id(),
                Some(&token),
            )?
            .send()
            .await
            .map_err(|_| transport())?;
        if !response.status().is_success() {
            response_value(response, LIMIT).await?;
            return Err(transport());
        }
        if response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            != Some("application/x-ndjson")
        {
            return Err(fail(
                426,
                "protocol_unsupported",
                "Device stream is unsupported.",
            ));
        }
        let mut frames = Frames {
            response,
            buffer: Vec::new(),
        };
        let first = frames.next().await?;
        if first["v"] != 1
            || first["kind"] != "connected"
            || first["body"]["deviceId"] != self.identity.device_id
            || first["body"]["generation"] != descriptor["deviceGeneration"]
            || first["body"]["authorityGeneration"] != descriptor["authorityGeneration"]
        {
            return Err(invalid("Stream binding does not match."));
        }
        let session = Session {
            token,
            connection: revision(&first["connectionGeneration"], 1)?,
            reports_providers: first["body"]["reportsProviders"] == true,
        };
        self.beat(&session).await?;
        Ok((session, frames))
    }

    async fn beat(&mut self, session: &Session) -> Result<()> {
        let zone = zone_now();
        let zone_revision = self.identity.zone_revision + i64::from(zone != self.identity.zone);
        let watermark: i64 = self.db.query_row(
            "SELECT journal_sequence FROM device_state WHERE id=1",
            [],
            |r| r.get(0),
        )?;
        let mut body = json!({"requestId":request_id(),"connectionGeneration":session.connection,
            "zone":zone,"zoneRevision":zone_revision,"journalWatermark":watermark});
        if session.reports_providers {
            if let Some((accounts, complete)) = self
                .provider
                .as_ref()
                .and_then(|provider| provider.metadata().ok().flatten())
            {
                body["providerInstances"] = accounts;
                body["providerInstancesComplete"] = json!(complete);
            }
        }
        self.post("heartbeat", &session.token, &body).await?;
        if self.identity.zone != zone {
            let mut identity = self.identity.clone();
            identity.zone = zone;
            identity.zone_revision = zone_revision;
            self.persist(&identity, true)?;
            self.identity = identity;
        }
        Ok(())
    }

    pub(super) async fn post(
        &mut self,
        operation: &str,
        token: &str,
        body: &Value,
    ) -> Result<Value> {
        let path = format!("/api/devices/{}/{operation}", self.identity.device_id);
        self.post_path(&path, token, body, None, LIMIT, TIMEOUT)
            .await
    }

    pub(super) async fn post_path(
        &mut self,
        path: &str,
        token: &str,
        body: &Value,
        executor: Option<&str>,
        limit: usize,
        timeout: Duration,
    ) -> Result<Value> {
        let data = body.to_string();
        let id = text(body, "requestId");
        tokio::time::timeout(timeout, async {
            loop {
                let mut signed = self.signed(path, Method::POST, data.clone(), id, Some(token))?;
                if let Some(executor) = executor {
                    signed = signed.header("x-minarbot-executor", executor);
                }
                match submit_bounded(signed, limit, timeout).await {
                    Err(err) if err.status == 429 && err.code == "rate_limited" => {
                        tokio::time::sleep(retry_delay(&err, 50)).await;
                    }
                    result => return result,
                }
            }
        })
        .await
        .map_err(|_| transport())?
    }
}

fn retry_delay(error: &Failure, default_ms: u64) -> Duration {
    Duration::from_millis(
        error
            .details
            .as_ref()
            .and_then(|details| details["retryAfterMs"].as_u64())
            .filter(|ms| *ms > 0)
            .unwrap_or(default_ms)
            .min(60_000),
    )
}

async fn connected(
    host: &mut Host,
    session: &Session,
    mut frames: Frames,
    recover: bool,
    on_recovery: &impl Fn(Vec<Value>),
) -> Result<()> {
    let mut waits = protected_wait::Stream::new();
    let period = Duration::from_secs(10);
    let mut heartbeat = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    resume_waits(&mut waits, host, session).await;
    if recover {
        if let Ok(results) = host.recover_journal(&session.token).await {
            on_recovery(results);
        }
    }
    loop {
        tokio::select! {
            frame = frames.next() => {
                let frame = frame?;
                if frame["v"] != 1 || frame["connectionGeneration"] != session.connection {
                    return Err(fail(426, "protocol_unsupported", "Device stream frame is unsupported."));
                }
                waits.handle(host, &session.token, session.connection, &frame).await?;
            }
            _ = heartbeat.tick() => {
                host.beat(session).await?;
                resume_waits(&mut waits, host, session).await;
                if recover {
                    if let Ok(results) = host.recover_journal(&session.token).await {
                        on_recovery(results);
                    }
                }
            }
        }
    }
}

async fn resume_waits(waits: &mut protected_wait::Stream, host: &mut Host, session: &Session) {
    if let Err(error) = waits.resume(host, &session.token).await {
        eprintln!("Device wait recovery deferred: {}", error.code);
    }
}

pub(super) async fn observe_task(host: &mut Host, session: &Session, frames: Frames) -> Failure {
    connected(host, session, frames, false, &|_| {})
        .await
        .err()
        .unwrap_or_else(transport)
}

pub(super) async fn run(host: Host) -> db::Result<()> {
    supervise(
        host,
        |phase, generation, _| println!("{}", json!({"phase":phase,"generation":generation})),
        |_| {},
    )
    .await
}

pub(super) async fn supervise(
    mut host: Host,
    on_state: impl Fn(&str, i64, Option<&Session>),
    on_recovery: impl Fn(Vec<Value>),
) -> db::Result<()> {
    if host.identity.descriptor.is_none() || host.identity.pending.is_some() {
        return Err("Device is not ready to connect".into());
    }
    let mut generation = 0;
    let mut failures = 0u32;
    let stopped = crate::shutdown();
    tokio::pin!(stopped);
    loop {
        generation += 1;
        on_state("connecting", generation, None);
        let opened = tokio::select! {
            _ = &mut stopped => break,
            opened = tokio::time::timeout(TIMEOUT, host.open_stream()) => opened.unwrap_or_else(|_| Err(transport())),
        };
        let (error, session_opened) = match opened {
            Err(error) => (error, false),
            Ok((session, frames)) => {
                failures = 0;
                on_state("connected", generation, Some(&session));
                let error = tokio::select! {
                    _ = &mut stopped => break,
                    error = connected(&mut host, &session, frames, true, &on_recovery) => error.err().unwrap_or_else(transport),
                };
                (error, true)
            }
        };
        if !(error.retryable || session_opened && error.status == 401) {
            on_state("blocked", generation, None);
            return Ok(());
        }
        failures += 1;
        let fallback = 1000u64 << failures.saturating_sub(1).min(6);
        on_state("backoff", generation, None);
        tokio::select! {
            _ = &mut stopped => break,
            _ = tokio::time::sleep(retry_delay(&error, fallback)) => {}
        }
    }
    on_state("offline", generation, None);
    Ok(())
}
