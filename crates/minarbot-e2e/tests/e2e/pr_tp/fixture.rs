//! Hosted server, device fetch proxy and device hosts (the Node `fixture()`
//! of `app/two-process.test.ts`).

use minarbot_testkit::{
    device_host::{DeviceHost, HostOptions},
    hosted::HostedApp,
    http::Method,
    prelude::*,
    relay::{self, Answer, Exchange, Relay, Upstream},
    wait::until_within,
    Servers,
};
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

/// Device hosts back off up to 60 s between reconnects, and a disconnect can
/// wait for the 30 s maintenance sweep, so device changes get one full step.
pub const DEVICE_RECONNECT: Duration = Duration::from_secs(65);
pub const SETTLE: Duration = Duration::from_secs(30);

pub fn locked<T>(state: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Poll until `check` holds (the Node `until`, 30 s by default).
pub async fn until<F, Fut>(label: &str, limit: Duration, mut check: F) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<bool>>,
{
    until_within(label, limit, || {
        let next = check();
        async move { Ok(next.await?.then_some(())) }
    })
    .await
}

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// What the device fetch proxy does with one request (the Node suites'
/// `globalThis.fetch` patches, which wrapped the proxy's upstream fetch).
pub enum Fault {
    /// Forward unchanged.
    Pass,
    /// Wait for this future, then forward.
    Hold(BoxFuture),
    /// Forward, read the reply, then lose it when `lose(status, body)` holds.
    LoseReply(Box<dyn Fn(u16, &[u8]) -> bool + Send>),
    /// Lose the request before it reaches the server.
    Drop,
    /// Forward, then reply with `rewrite(status, body)` instead.
    Rewrite(Box<dyn Fn(u16, Vec<u8>) -> Vec<u8> + Send>),
    /// Forward and read the reply, then deliver it after this future.
    After(BoxFuture),
}

pub type FaultHook = Arc<dyn Fn(&Exchange) -> Fault + Send + Sync>;

pub struct Tp {
    pub app: HostedApp,
    pub proxy: Relay,
    pub hosts: Mutex<Vec<Arc<DeviceHost>>>,
    hook: Arc<Mutex<Option<FaultHook>>>,
    target: Arc<Mutex<String>>,
}

impl Tp {
    /// Start a hosted server for a case that runs `servers` servers.
    pub async fn start() -> Result<Self> {
        let servers = Servers::reserve(1).await?;
        Self::start_in(&servers).await
    }

    pub async fn start_in(servers: &Servers) -> Result<Self> {
        Self::start_env(servers, &[]).await
    }

    /// Start with extra server environment (for example `TZ`).
    pub async fn start_env(servers: &Servers, env: &[(&str, &str)]) -> Result<Self> {
        let app = HostedApp::start_env(TestRoot::new()?, servers, env).await?;
        let target = Arc::new(Mutex::new(app.origin()));
        let hook: Arc<Mutex<Option<FaultHook>>> = Arc::default();
        let (shared, faults) = (target.clone(), hook.clone());
        let proxy = Relay::start(move |exchange| {
            let (shared, faults) = (shared.clone(), faults.clone());
            async move {
                let origin = locked(&shared).clone();
                let fault = locked(&faults)
                    .clone()
                    .map_or(Fault::Pass, |hook| hook(&exchange));
                let mut exchange = exchange;
                for (name, value) in &mut exchange.headers {
                    if name == "origin" {
                        *value = origin.clone();
                    }
                }
                if let Fault::Hold(wait) = fault {
                    wait.await;
                    return forward(&exchange, &origin).await;
                }
                if let Fault::Drop = fault {
                    return Ok(Answer::Drop);
                }
                let upstream = match exchange.forward(&origin).await {
                    Ok(upstream) => upstream,
                    Err(_) => return Ok(Answer::Drop),
                };
                if let Fault::After(wait) = fault {
                    let status = upstream.status;
                    let headers = relay::without_length(upstream.headers.clone());
                    let body = upstream.bytes().await.unwrap_or_default();
                    wait.await;
                    return Ok(Answer::bytes(status, headers, body));
                }
                if let Fault::Rewrite(rewrite) = fault {
                    let status = upstream.status;
                    let headers = relay::without_length(upstream.headers.clone());
                    let body = upstream.bytes().await.unwrap_or_default();
                    return Ok(Answer::bytes(status, headers, rewrite(status, body)));
                }
                if let Fault::LoseReply(lose) = fault {
                    let status = upstream.status;
                    let headers = relay::without_length(upstream.headers.clone());
                    let body = upstream.bytes().await.unwrap_or_default();
                    if lose(status, &body) {
                        return Ok(Answer::Drop);
                    }
                    return Ok(Answer::bytes(status, headers, body));
                }
                Ok(relayed(upstream))
            }
        })
        .await?;
        Ok(Self {
            app,
            proxy,
            hosts: Mutex::new(Vec::new()),
            hook,
            target,
        })
    }

    /// Install or clear the device fetch fault hook.
    pub fn fault(&self, hook: Option<FaultHook>) {
        *locked(&self.hook) = hook;
    }

    pub fn origin(&self) -> String {
        self.app.origin()
    }

    pub fn dir(&self) -> PathBuf {
        self.app.root.path().to_owned()
    }

    pub fn database(&self) -> PathBuf {
        self.app.database()
    }

    pub fn jwt(&self, subject: &str) -> Result<String> {
        self.app.token(subject)
    }

    /// Owner request (the Node `f.send(path, body)`).
    pub async fn send(&self, path: &str, body: Option<Value>) -> Result<(u16, Value)> {
        self.app.send(path, body.as_ref(), "user_owner", None).await
    }

    pub async fn send_as(
        &self,
        path: &str,
        body: Option<Value>,
        subject: &str,
        method: Option<Method>,
    ) -> Result<(u16, Value)> {
        self.app.send(path, body.as_ref(), subject, method).await
    }

    /// `data` of an owner GET that must succeed.
    pub async fn data(&self, path: &str) -> Result<Value> {
        let (status, body) = self.send(path, None).await?;
        ensure!(status == 200, "{path}: {status} {body}");
        Ok(body["data"].clone())
    }

    /// A device host whose database lives in this fixture's folder and
    /// which reaches the server through the device fetch proxy.
    pub fn device(&self, name: &str) -> Result<Arc<DeviceHost>> {
        let host = Arc::new(DeviceHost::new(HostOptions::new(
            &self.dir().join(name),
            self.proxy.origin(),
        )?));
        locked(&self.hosts).push(host.clone());
        Ok(host)
    }

    pub async fn restart(&mut self) -> Result<()> {
        self.app.restart().await?;
        *locked(&self.target) = self.app.origin();
        Ok(())
    }

    pub async fn restore_from(&mut self, backup: &std::path::Path) -> Result<Value> {
        self.app.restore_from(backup).await
    }

    /// Close device hosts (the server stops with the fixture).
    pub async fn close(&self) -> Result<()> {
        let hosts: Vec<_> = locked(&self.hosts).drain(..).collect();
        for host in hosts {
            host.close().await?;
        }
        Ok(())
    }
}

async fn forward(exchange: &Exchange, origin: &str) -> Result<Answer> {
    match exchange.forward(origin).await {
        Ok(upstream) => Ok(relayed(upstream)),
        Err(_) => Ok(Answer::Drop),
    }
}

/// Reply passed through without length or encoding headers, as the Node
/// proxy wrote it.
fn relayed(upstream: Upstream) -> Answer {
    let status = upstream.status;
    let headers: Vec<(String, String)> = relay::without_length(upstream.headers.clone())
        .into_iter()
        .filter(|(name, _)| name != "content-encoding")
        .collect();
    Answer::stream(status, headers, upstream.stream())
}

/// `digestCommand` of the Node protocol module.
pub fn digest(value: &Value) -> String {
    minarbot_testkit::protocol::digest_command(value)
}

/// Copy the running canonical database into a backup snapshot file with the
/// backup marker tables (the Node `workspaceSnapshot`). Returns its path.
pub fn workspace_snapshot(tp: &Tp, owner: Option<&str>) -> Result<PathBuf> {
    let file = tp.dir().join(format!("snapshot-{}.sqlite", &new_id()[..8]));
    let source = minarbot_testkit::db::Fixture::open(tp.database())?;
    source.execute("VACUUM INTO ?", [file.to_string_lossy().as_ref()])?;
    drop(source);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
    let snapshot = minarbot_testkit::db::Fixture::open(&file)?;
    snapshot.execute_batch(
        "CREATE TABLE minarbot_backup(format TEXT NOT NULL,version INTEGER NOT NULL,created TEXT NOT NULL) STRICT;
         CREATE TABLE minarbot_backup_authority(generation TEXT NOT NULL,owner_account TEXT REFERENCES accounts(id)) STRICT;",
    )?;
    snapshot.execute(
        "INSERT INTO minarbot_backup VALUES ('minarbot-canonical-v6',6,?)",
        ["2026-11-25T00:00:00.000Z"],
    )?;
    snapshot.execute(
        "INSERT INTO minarbot_backup_authority SELECT generation,coalesce(?,(SELECT min(account) FROM projects HAVING count(DISTINCT account)=1)) FROM authority_state WHERE id=1",
        [owner],
    )?;
    Ok(file)
}

/// Standard agent creation body for a home device.
pub fn agent_body(
    agent: &str,
    name: &str,
    instructions: &str,
    project: &str,
    home: &Value,
) -> Value {
    json!({"requestId": agent, "id": agent, "name": name, "model": "fixture", "backupModel": "",
        "instructions": instructions, "threadLimit": 2, "projectId": project,
        "homeDeviceId": home["id"], "expectedProjectRevision": 1,
        "expectedHomeDeviceRevision": home["revision"], "providerBinding": null})
}

/// Expected revisions of a fresh project/agent/home binding.
pub fn fresh_revisions() -> Value {
    json!({"project": 1, "agent": 1, "homeBinding": 1, "membership": 1, "assignment": 1,
        "capability": 1})
}

/// Task admission body with its payload digest.
pub fn task_body(task: &str, project: &str, agent: &str, home: &str, payload: &Value) -> Value {
    let revisions = fresh_revisions();
    json!({"requestId": task, "projectId": project, "agentId": agent, "homeDeviceId": home,
        "payload": payload, "expectedRevisions": revisions,
        "payloadDigest": digest(&json!(["minarbot-task-payload-v1", task, project, agent, home,
            payload, revisions]))})
}

/// `{v:1, kind:'direct', message:{text, files:[]}}`.
pub fn direct(text: &str) -> Value {
    json!({"v": 1, "kind": "direct", "message": {"text": text, "files": []}})
}

/// An enrolled, connected home device.
pub struct Home {
    pub host: Arc<minarbot_testkit::device_host::DeviceHost>,
    pub connection: minarbot_testkit::device_host::Connection,
    pub device: Value,
}

impl Home {
    pub fn id(&self) -> String {
        self.device["id"].as_str().unwrap_or_default().to_owned()
    }
}

/// A task this home owns (`startTask` plus the action sequence).
pub struct Owned {
    pub task: String,
    pub ownership: Value,
    pub revision: Value,
    pub sequence: i64,
}

impl Tp {
    /// Enroll and connect a home in `name`; `device` is its `/api/devices` row.
    pub async fn home(&self, name: &str) -> Result<Home> {
        let host = self.device(name)?;
        host.enroll(&self.jwt("user_owner")?, "new").await?;
        let connection = host.connect().await?;
        until("home connected", SETTLE, || async {
            Ok(connection.phase() == "connected")
        })
        .await?;
        let id = host.device_id()?;
        let devices = self.data("/api/devices").await?;
        let device = devices
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["id"] == id.as_str()).cloned())
            .unwrap_or_else(|| devices[0].clone());
        Ok(Home {
            host,
            connection,
            device,
        })
    }

    pub async fn project(&self, id: &str, name: &str) -> Result<()> {
        let (status, body) = self
            .send(
                "/api/projects",
                Some(json!({"requestId": new_id(), "id": id, "name": name})),
            )
            .await?;
        ensure!(status == 201, "project: {status} {body}");
        Ok(())
    }

    pub async fn agent(
        &self,
        agent: &str,
        name: &str,
        instructions: &str,
        project: &str,
        home: &Home,
    ) -> Result<()> {
        let (status, body) = self
            .send(
                "/api/agents",
                Some(agent_body(agent, name, instructions, project, &home.device)),
            )
            .await?;
        ensure!(status == 201, "agent: {status} {body}");
        Ok(())
    }

    /// Admit a direct task (202 required).
    pub async fn task(
        &self,
        task: &str,
        project: &str,
        agent: &str,
        home: &Home,
        text: &str,
    ) -> Result<()> {
        let (status, body) = self
            .send(
                "/api/tasks",
                Some(task_body(task, project, agent, &home.id(), &direct(text))),
            )
            .await?;
        ensure!(status == 202, "task admission: {status} {body}");
        Ok(())
    }

    pub async fn task_detail(&self, task: &str) -> Result<Value> {
        self.data(&format!("/api/tasks/{task}")).await
    }

    /// Owner database (read-only).
    pub fn inspect(&self) -> Result<minarbot_testkit::db::Inspector> {
        minarbot_testkit::db::Inspector::open(self.database())
    }

    /// Owner database (writable, for seeding and fault triggers).
    pub fn write(&self) -> Result<minarbot_testkit::db::Fixture> {
        minarbot_testkit::db::Fixture::open(self.database())
    }

    /// Lose the reply of the next request whose path ends with `suffix`
    /// (after checking the server answered 200).
    pub fn lose_next(&self, suffix: &'static str) -> Arc<Mutex<Option<u16>>> {
        let seen: Arc<Mutex<Option<u16>>> = Arc::default();
        let (status_seen, armed) = (
            seen.clone(),
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
        );
        self.fault(Some(Arc::new(move |exchange| {
            if exchange.path.ends_with(suffix)
                && armed.swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                let status_seen = status_seen.clone();
                return Fault::LoseReply(Box::new(move |status, _| {
                    *locked(&status_seen) = Some(status);
                    true
                }));
            }
            Fault::Pass
        })));
        seen
    }

    /// Restart and wait until `home` reconnected with a newer generation.
    pub async fn restart_home(&mut self, home: &Home) -> Result<()> {
        let generation = home.connection.generation();
        self.restart().await?;
        until("home reconnected", SETTLE, || async {
            Ok(home.connection.phase() == "connected" && home.connection.generation() > generation)
        })
        .await
    }
}

impl Home {
    pub async fn start(&self, task: &str) -> Result<Owned> {
        let owned = self.host.start_task(task).await?;
        Ok(Owned {
            task: task.to_owned(),
            ownership: owned["ownership"].clone(),
            revision: owned["revision"].clone(),
            sequence: 0,
        })
    }

    /// Build a canonical tool-call action at the task's current revision.
    pub async fn canonical_input(
        &self,
        tp: &Tp,
        owned: &mut Owned,
        name: &str,
        args: &Value,
    ) -> Result<Value> {
        let detail = tp.task_detail(&owned.task).await?;
        let context = self
            .host
            .context(&owned.task, &owned.ownership, &detail["revision"])
            .await?;
        let action = new_id();
        let sequence = owned.sequence;
        owned.sequence += 1;
        Ok(
            json!({"taskId": owned.task, "actionId": action, "sequence": sequence,
            "actionKind": "canonical", "ownership": owned.ownership,
            "payload": {"toolCall": {"id": action, "function": {"name": name,
                "arguments": args.to_string()}},
                "expectedRevision": context["snapshot"]["taskRevision"].as_i64().unwrap_or(0) + 1},
            "context": context, "grantRevision": 1}),
        )
    }

    /// Run a canonical action; it must not reach the host provider.
    pub async fn execute(&self, input: &Value) -> Result<Value> {
        self.host
            .action(input, |_| async { Err("Host effect".into()) })
            .await
    }

    /// Wait until recovery acknowledged `action` (15 s, as in Node).
    pub async fn acknowledged(&self, action: &str) -> Result<()> {
        until(
            "action recovery acknowledged",
            Duration::from_secs(15),
            || async {
                Ok(self.host.recovery_status().as_array().is_some_and(|rows| {
                    rows.iter()
                        .any(|row| row["actionId"] == action && row["state"] == "acknowledged")
                }))
            },
        )
        .await
    }

    /// Result content of `action` in the task detail.
    pub async fn content(&self, tp: &Tp, task: &str, action: &str) -> Result<Value> {
        let detail = tp.task_detail(task).await?;
        detail["results"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["actionId"] == action))
            .map(|row| row["content"].clone())
            .ok_or_else(|| format!("no result for {action}: {detail}").into())
    }
}

/// `encodeURIComponent`.
pub fn encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Session account id of `subject`.
pub async fn account(tp: &Tp, subject: &str) -> Result<String> {
    Ok(
        tp.send_as("/api/session", None, subject, None).await?.1["data"]["accountId"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    )
}
