//! Self-tests for kit pieces that need no Minarbot server.

use minarbot_testkit::{
    bridge::{Bridge, BridgeError},
    ensure,
    fakes::{self, Fake},
    issuer::SessionIssuer,
    model::{Reply, ScriptedModel},
    protocol::{canonical_json, digest_command},
    proxy::LossyProxy,
    server::Gateway,
    Result, ServerConfig, FIXED_CLOCK,
};
use serde_json::{json, Value};
use std::path::Path;

#[test]
fn config_serializes_like_the_node_bridge() -> Result<()> {
    let database = Path::new("/tmp/state.sqlite");
    let error = ServerConfig::new()
        .option("unported", true)
        .to_json(database);
    ensure!(
        error.is_err_and(|e| e.to_string() == "not ported: option unported"),
        "unknown option accepted"
    );
    let config = ServerConfig::new()
        .gateway(Gateway {
            api_key: Some("key".into()),
            default_model: Some("work".into()),
            stream: Some(true),
            ..Gateway::new("http://127.0.0.1:9/v1")
        })
        .workspace("agent", "/tmp/w", true)
        .executable("claude", "/bin/claude-fake")
        .path_prefix("/opt/bin")
        .sync_key([7; 32])
        .to_json(database)?;
    ensure!(
        config["fixedClock"] == FIXED_CLOCK && config["port"] == 0 && config["host"] == "127.0.0.1",
        "defaults: {config}"
    );
    ensure!(
        config["database"] == "/tmp/state.sqlite" && config.get("testPathPrefix").is_none(),
        "paths: {config}"
    );
    let gateway = &config["harness"]["testGateway"];
    ensure!(
        gateway["baseURL"] == "http://127.0.0.1:9/v1"
            && gateway["apiKey"] == "key"
            && gateway["defaultModel"] == "work"
            && gateway["stream"] == true,
        "gateway: {gateway}"
    );
    ensure!(
        config["harness"]["executables"]["codex"] == json!(fakes::path(Fake::Gateway)?),
        "codex runs the gateway: {config}"
    );
    ensure!(
        config["harness"]["executables"]["claude"] == "/bin/claude-fake",
        "explicit executable kept"
    );
    ensure!(
        config["workspaces"] == json!([{"agent": "agent", "directory": "/tmp/w", "write": true}]),
        "workspaces"
    );
    ensure!(
        config["testKeyring"]["sync-hmac/v1"] == "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc",
        "sync key: {}",
        config["testKeyring"]
    );
    let legacy = ServerConfig::new()
        .option(
            "accounts",
            json!([{"baseURL": "http://127.0.0.1:8/v1", "apiKey": "a"}]),
        )
        .to_json(database)?;
    ensure!(
        legacy["harness"]["testGateway"]["baseURL"] == "http://127.0.0.1:8/v1",
        "legacy account gateway: {legacy}"
    );
    ensure!(
        ServerConfig::new()
            .real_clock()
            .to_json(database)?
            .get("fixedClock")
            .is_none(),
        "real clock"
    );
    Ok(())
}

#[tokio::test]
async fn config_wires_issuer_and_bridges() -> Result<()> {
    let issuer = SessionIssuer::start().await?;
    let computer = Bridge::operations(|operation, _| match operation {
        "clock" => Ok(json!(1)),
        _ => Err(BridgeError::new(404, "unknown")),
    })
    .await?;
    let pdf = Bridge::pdf(|_| Some((503, "busy".into()))).await?;
    let key = Bridge::composio_key("composio-key").await?;
    let canonical = Bridge::paths(|_, _| Ok(json!(null))).await?;
    let config = ServerConfig::new()
        .issuer(&issuer)
        .computer_bridge(&computer)
        .pdf_bridge(&pdf)
        .composio("http://127.0.0.1:7", &key)
        .canonical_bridge(&canonical, true, false, true);
    ensure!(
        config.computer_bridge_token() == Some(computer.token()),
        "computer token"
    );
    let json = config.to_json(Path::new("/tmp/db.sqlite"))?;
    ensure!(
        json["testHarnessBridge"]["origin"].is_string(),
        "issuer: {json}"
    );
    ensure!(
        json["testComputerBridge"] == computer.config() && json["testPdfBridge"] == pdf.config(),
        "bridges: {json}"
    );
    ensure!(
        json["testConnectorBridge"] == key.config()
            && json["composio"]["baseURL"] == "http://127.0.0.1:7",
        "composio: {json}"
    );
    ensure!(
        json["testCanonicalBridge"]["clerk"] == true
            && json["testCanonicalBridge"]["mail"] == false,
        "canonical: {json}"
    );
    let client = reqwest::Client::new();
    let reply: Value = client
        .post(computer.origin())
        .bearer_auth(computer.token())
        .json(&json!({"operation": "clock", "args": {}}))
        .send()
        .await?
        .json()
        .await?;
    ensure!(reply == json!({"value": 1}), "computer bridge: {reply}");
    let denied = client
        .post(computer.origin())
        .json(&json!({"operation": "clock"}))
        .send()
        .await?;
    ensure!(
        denied.status().as_u16() == 403,
        "unauthenticated bridge call: {}",
        denied.status()
    );
    let fault = client
        .post(pdf.origin())
        .bearer_auth(pdf.token())
        .body("pdf")
        .send()
        .await?;
    ensure!(fault.status().as_u16() == 503, "pdf fault status");
    let served: Value = client
        .get(format!("{}/composio-key", key.origin()))
        .bearer_auth(key.token())
        .send()
        .await?
        .json()
        .await?;
    ensure!(served["key"] == "composio-key", "composio key: {served}");
    let minted: Value = client
        .post(
            json["testHarnessBridge"]["origin"]
                .as_str()
                .unwrap_or_default(),
        )
        .bearer_auth(
            json["testHarnessBridge"]["token"]
                .as_str()
                .unwrap_or_default(),
        )
        .json(&json!({"operation": "mint", "args": {"agent": "a", "turn": "t"}}))
        .send()
        .await?
        .json()
        .await?;
    ensure!(
        minted["value"]["token"] == minarbot_testkit::issuer::SESSION_TOKEN,
        "mint: {minted}"
    );
    ensure!(
        issuer.minted().len() == 1 && issuer.minted()[0].turn == "t",
        "issuer log"
    );
    Ok(())
}

#[tokio::test]
async fn scripted_model_queues_responds_streams_records_and_holds() -> Result<()> {
    let model = ScriptedModel::start().await?;
    let client = reqwest::Client::new();
    let url = format!("{}/chat/completions", model.base_url());
    let empty: Value = client
        .post(&url)
        .json(&json!({"messages": []}))
        .send()
        .await?
        .json()
        .await?;
    ensure!(
        empty["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("no reply")),
        "unscripted request: {empty}"
    );
    model.push(Reply::tool("echo", json!({"a": 1})).usage(3, 4));
    model.respond(|request| Reply::text(format!("reply:{}", request.last_text())));
    let tool: Value = client
        .post(&url)
        .json(&json!({"model": "m", "messages": [{"role": "user", "content": "x"}]}))
        .send()
        .await?
        .json()
        .await?;
    ensure!(
        tool["choices"][0]["finish_reason"] == "tool_calls"
            && tool["choices"][0]["message"]["tool_calls"][0]["function"]["name"] == "echo",
        "tool: {tool}"
    );
    ensure!(tool["usage"]["total_tokens"] == 7, "usage: {tool}");
    let text: Value = client
        .post(&url)
        .json(&json!({"messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await?
        .json()
        .await?;
    ensure!(
        text["choices"][0]["message"]["content"] == "reply:hi",
        "responder: {text}"
    );
    model.push(Reply::deltas(["a", "b"]));
    let stream = client
        .post(&url)
        .json(&json!({"stream": true, "messages": []}))
        .send()
        .await?
        .text()
        .await?;
    ensure!(
        stream.contains("\"content\":\"a\"")
            && stream.contains("\"content\":\"b\"")
            && stream.ends_with("data: [DONE]\n\n"),
        "stream: {stream}"
    );
    model.push(Reply::status(429, json!({"error": "slow down"})));
    ensure!(
        client
            .post(&url)
            .json(&json!({}))
            .send()
            .await?
            .status()
            .as_u16()
            == 429,
        "status reply"
    );
    ensure!(
        model.calls() == 5 && model.requests()[1].model() == "m",
        "recorded requests"
    );
    model.catalog(["a", "b"]);
    let models: Value = client
        .get(format!("{}/models", model.base_url()))
        .send()
        .await?
        .json()
        .await?;
    ensure!(models["data"][1]["id"] == "b", "catalog: {models}");
    model.fail_catalog(true);
    ensure!(
        client
            .get(format!("{}/models", model.base_url()))
            .send()
            .await?
            .status()
            .as_u16()
            == 502,
        "failing catalog"
    );
    model.push(Reply::hold());
    let held = tokio::spawn({
        let client = client.clone();
        let url = url.clone();
        async move {
            client
                .post(url)
                .json(&json!({}))
                .send()
                .await
                .map(|r| r.status().as_u16())
        }
    });
    minarbot_testkit::wait::until("held request", || async {
        Ok((model.held() == 1).then_some(()))
    })
    .await?;
    drop(model);
    let status = held.await?.ok();
    ensure!(
        status.is_none_or(|code| code == 503),
        "held request after stop: {status:?}"
    );
    Ok(())
}

#[tokio::test]
async fn lossy_proxy_forwards_records_drops_and_goes_offline() -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(|_| Reply::text("ok"));
    let proxy = LossyProxy::start(model.origin()).await?;
    let client = reqwest::Client::new();
    let url = format!("{}/v1/chat/completions", proxy.origin());
    let reply: Value = client
        .post(&url)
        .bearer_auth("t")
        .json(&json!({"messages": []}))
        .send()
        .await?
        .json()
        .await?;
    ensure!(
        reply["choices"][0]["message"]["content"] == "ok",
        "forwarded: {reply}"
    );
    ensure!(
        model.requests()[0].authorization.as_deref() == Some("Bearer t"),
        "authorization forwarded"
    );
    proxy.drop_next(|request| request.method == "POST" && request.ok());
    ensure!(
        client.post(&url).json(&json!({})).send().await.is_err(),
        "dropped reply was delivered"
    );
    ensure!(
        model.calls() == 2 && proxy.pending_rules() == 0,
        "drop rule must forward first"
    );
    proxy.set_offline(true);
    ensure!(
        client.post(&url).json(&json!({})).send().await.is_err(),
        "offline proxy answered"
    );
    ensure!(model.calls() == 2, "offline proxy forwarded");
    proxy.set_offline(false);
    let requests = proxy.requests();
    ensure!(
        requests.len() == 3 && requests[1].dropped && requests[2].status.is_none(),
        "log: {requests:?}"
    );
    Ok(())
}

#[test]
fn digests_match_the_protocol() -> Result<()> {
    ensure!(
        canonical_json(&json!({"b": [1, {"d": 1, "c": "x"}], "a": null}))
            == r#"{"a":null,"b":[1,{"c":"x","d":1}]}"#,
        "canonical JSON"
    );
    ensure!(
        digest_command(&json!("x"))
            == "ba2df4903a2c14e86dc3bcca58911b44ac1d2514b7227bf6eb08cfb978f55a1b",
        "digest of \"x\""
    );
    Ok(())
}
