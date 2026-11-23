use super::*;
use std::{
    io::{self, BufRead, Write},
    sync::atomic::{AtomicI64, Ordering},
};
use tokio::sync::oneshot;

static CLOCK: AtomicI64 = AtomicI64::new(0);

pub(super) fn clock() -> Option<i64> {
    let value = CLOCK.load(Ordering::Relaxed);
    (value != 0).then_some(value)
}

pub(super) fn validate(config: &Config) -> crate::db::Result<()> {
    if std::env::var("MINARBOT_TEST_SERVER").as_deref() != Ok("rust")
        || !matches!(config.identity, session::IdentityConfig::Stub { .. })
        || config.port != 0
        || !config.trusted_gateway_addresses.is_empty()
    {
        return Err(
            "Hosted hub test fixture requires stub identity and an ephemeral loopback listener."
                .into(),
        );
    }
    Ok(())
}

pub(super) fn start(hub: &Arc<Hub>) -> oneshot::Receiver<crate::db::Result<()>> {
    let hub = Arc::downgrade(hub);
    let (stop, receiver) = oneshot::channel();
    std::thread::spawn(move || {
        let result = (|| {
            for line in io::stdin().lock().lines() {
                let line = line?;
                let Some(hub) = hub.upgrade() else { break };
                let response = match control(&hub, &line) {
                    Ok(result) => json!({"result":result}),
                    Err(error) => {
                        json!({"error":{"status":error.status,"code":error.code,"message":error.message}})
                    }
                };
                writeln!(io::stdout(), "{response}")?;
            }
            Ok(())
        })();
        let _ = stop.send(result);
    });
    receiver
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    now: i64,
    post: Option<Post>,
    withdraw_node: Option<Withdrawal>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Post {
    project_id: String,
    account_id: String,
    input: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Withdrawal {
    principal: Value,
    input: Value,
}

// Keep the clock update and post under one lock to observe queue expiry before
// the socket pump fences the idle connection.
fn control(hub: &Hub, line: &str) -> Result<Value> {
    let command: Command =
        serde_json::from_value(parse_json(line.as_bytes())?).map_err(|_| denied())?;
    let mut db = hub.lock()?;
    if !(1..=9_007_199_254_740_991).contains(&command.now)
        || clock().is_some_and(|previous| command.now < previous)
    {
        return Err(denied());
    }
    CLOCK.store(command.now, Ordering::Relaxed);
    if let Some(withdrawal) = command.withdraw_node {
        if command.post.is_some() {
            return Err(denied());
        }
        return transaction(&mut db, |db| {
            authority::withdraw_node(db, &withdrawal.principal, &withdrawal.input)?;
            Ok(Value::Null)
        });
    }
    let Some(post) = command.post else {
        return Ok(Value::Null);
    };
    transaction(&mut db, |db| {
        let project = id(&json!(post.project_id))?;
        let account = id(&json!(post.account_id))?;
        let host = authority::context_host(db, &project)?;
        let ctx = authority::Context {
            project: &project,
            account: &account,
            host: &host,
        };
        routing::post(db, &ctx, &post.input)
    })
}
