//! Provider sign-in from a browser on another machine, setup's detected
//! providers, and Codex homes that run on an API-key provider.
//!
//! The fake CLIs print their sign-in link the way Codex and Claude Code do:
//! Codex a device link and one-time code, Claude a link and a prompt for the
//! pasted code (`native::SIGN_IN_CODE`).

use minarbot_testkit::{
    fake::native::{DEVICE_CODE, SIGN_IN_CODE},
    fakes::{self, Fake},
    prelude::*,
};

async fn instance(app: &Server, id: &str, fake: Fake, home: &std::path::Path) -> Result<Value> {
    let kind = if fake == Fake::Claude {
        "claude"
    } else {
        "codex"
    };
    app.post_expect(
        "/api/provider-instances",
        json!({"id": new_id(), "instanceId": id, "kind": kind, "displayName": id,
            "enabled": true, "binaryPath": fakes::path(fake)?, "homePath": home}),
        201,
    )
    .await
}

fn row<'a>(list: &'a Value, id: &str) -> Result<&'a Value> {
    list["providers"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == id))
        .ok_or_else(|| format!("{id} missing: {list}").into())
}

fn message(body: &Value) -> String {
    body["error"]["message"]
        .as_str()
        .or_else(|| body["error"].as_str())
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn codex_sign_in_returns_the_device_link_and_code_and_signs_the_home_in() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let home = root.dir("codex-home")?;
    let created = instance(&app, "codex-device", Fake::Codex, &home).await?;
    ensure!(
        created["auth"]["status"] == "unauthenticated",
        "before: {created}"
    );

    let started = app
        .post_expect(
            "/api/provider-instances/codex-device/sign-in",
            json!({"id": new_id(), "instanceId": "codex-device"}),
            200,
        )
        .await?;
    ensure!(
        started["url"] == "https://auth.openai.com/codex/device"
            && started["code"] == DEVICE_CODE
            && started["needsCode"] == false,
        "started: {started}"
    );
    let list = app
        .wait_get("/api/provider-instances", |list| {
            row(list, "codex-device").is_ok_and(|row| row["auth"]["status"] == "authenticated")
        })
        .await?;
    ensure!(
        row(&list, "codex-device")?["auth"]["method"] == "ChatGPT",
        "after: {list}"
    );
    ensure!(
        home.join("auth.json").is_file(),
        "auth.json not in the instance home"
    );
    Ok(())
}

#[tokio::test]
async fn claude_sign_in_takes_the_pasted_code_and_reports_a_rejected_one() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let home = root.dir("claude-home")?;
    instance(&app, "claude-paste", Fake::Claude, &home).await?;
    let start = || {
        app.post_expect(
            "/api/provider-instances/claude-paste/sign-in",
            json!({"id": new_id(), "instanceId": "claude-paste"}),
            200,
        )
    };
    let code = |code: &str| json!({"id": new_id(), "instanceId": "claude-paste", "code": code});
    let path = "/api/provider-instances/claude-paste/sign-in-code";

    let started = start().await?;
    ensure!(
        started["url"] == "https://claude.ai/oauth/authorize?code=true&state=fixture"
            && started["code"].is_null()
            && started["needsCode"] == true,
        "started: {started}"
    );
    let rejected = app.post_expect(path, code("wrong"), 400).await?;
    ensure!(
        message(&rejected) == "That code wasn't accepted.",
        "rejected: {rejected}"
    );
    // The CLI exited on the rejected code: a second paste needs a new sign-in.
    let expired = app.post_expect(path, code(SIGN_IN_CODE), 409).await?;
    ensure!(
        message(&expired) == "This sign-in expired. Start it again.",
        "expired: {expired}"
    );
    ensure!(
        !home.join(".credentials.json").exists(),
        "signed in on a rejected code"
    );

    start().await?;
    let signed = app.post_expect(path, code(SIGN_IN_CODE), 200).await?;
    ensure!(
        signed["id"] == "claude-paste"
            && signed["auth"]["status"] == "authenticated"
            && signed["auth"]["method"] == "Claude",
        "signed in: {signed}"
    );
    ensure!(
        home.join(".credentials.json").is_file(),
        "credentials not in the instance home"
    );
    Ok(())
}

#[tokio::test]
async fn a_new_sign_in_replaces_the_pending_one() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let home = root.dir("claude-home")?;
    instance(&app, "claude-twice", Fake::Claude, &home).await?;
    for _ in 0..2 {
        app.post_expect(
            "/api/provider-instances/claude-twice/sign-in",
            json!({"id": new_id(), "instanceId": "claude-twice"}),
            200,
        )
        .await?;
    }
    let signed = app
        .post_expect(
            "/api/provider-instances/claude-twice/sign-in-code",
            json!({"id": new_id(), "instanceId": "claude-twice", "code": SIGN_IN_CODE}),
            200,
        )
        .await?;
    ensure!(
        signed["auth"]["status"] == "authenticated",
        "signed in: {signed}"
    );
    Ok(())
}

#[tokio::test]
async fn sign_in_code_needs_a_started_sign_in_and_a_code() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let home = root.dir("claude-home")?;
    instance(&app, "claude-idle", Fake::Claude, &home).await?;
    let path = "/api/provider-instances/claude-idle/sign-in-code";
    let idle = app
        .post_expect(
            path,
            json!({"id": new_id(), "instanceId": "claude-idle", "code": SIGN_IN_CODE}),
            409,
        )
        .await?;
    ensure!(
        message(&idle) == "This sign-in expired. Start it again.",
        "idle: {idle}"
    );
    let blank = app
        .post_expect(
            path,
            json!({"id": new_id(), "instanceId": "claude-idle", "code": "  "}),
            400,
        )
        .await?;
    ensure!(
        message(&blank) == "Paste the code from the sign-in page.",
        "blank: {blank}"
    );
    Ok(())
}

#[tokio::test]
async fn codex_home_on_an_api_key_provider_is_signed_in_without_chatgpt() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new()).await?;
    let api = root.dir("codex-api")?;
    std::fs::write(
        api.join("config.toml"),
        "model = \"m\"\nmodel_provider = \"router\"\n[model_providers.router]\nname = \"Router\"\nbase_url = \"https://router.invalid/v1\"\n",
    )?;
    let openai = root.dir("codex-openai")?;
    std::fs::write(
        openai.join("config.toml"),
        "model_provider = \"proxy\"\n[model_providers.proxy]\nname = \"Proxy\"\nrequires_openai_auth = true\n",
    )?;
    let created = instance(&app, "codex-api", Fake::Codex, &api).await?;
    ensure!(
        created["auth"]["status"] == "authenticated" && created["auth"]["method"] == "Router",
        "api-key provider: {created}"
    );
    let created = instance(&app, "codex-openai", Fake::Codex, &openai).await?;
    ensure!(
        created["auth"]["status"] == "unauthenticated",
        "provider needing OpenAI auth: {created}"
    );
    Ok(())
}

#[tokio::test]
async fn detected_offers_each_installed_kind_until_it_has_an_instance() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root
        .start(
            ServerConfig::new()
                .executable("claude", fakes::path(Fake::Claude)?)
                .executable("hermes", fakes::path(Fake::Acp)?),
        )
        .await?;
    let detected = app
        .get_expect("/api/provider-instances/detected", 200)
        .await?;
    let kinds = detected["providers"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| row["kind"].clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    // Claude comes first, so setup picks it before other usable kinds.
    ensure!(
        kinds == [json!("claude"), json!("hermes")],
        "detected: {detected}"
    );
    let claude = row(&detected, "claude")?;
    ensure!(
        claude["displayName"] == "Claude"
            && claude["installed"] == true
            && claude["auth"]["status"] == "unauthenticated",
        "claude: {detected}"
    );
    let hermes = row(&detected, "hermes")?;
    ensure!(
        hermes["displayName"] == "Hermes"
            && hermes["installed"] == true
            && hermes["enabled"] == true,
        "hermes: {detected}"
    );

    // An instance of the kind under any ID stops the offer.
    app.post_expect(
        "/api/provider-instances",
        json!({"id": new_id(), "instanceId": "work-claude", "kind": "claude",
            "displayName": "Work Claude", "enabled": true}),
        201,
    )
    .await?;
    let detected = app
        .get_expect("/api/provider-instances/detected", 200)
        .await?;
    ensure!(
        row(&detected, "claude").is_err(),
        "still offered: {detected}"
    );
    ensure!(
        row(&detected, "hermes").is_ok(),
        "hermes dropped: {detected}"
    );
    Ok(())
}

/// An instance whose ID is its kind is an ordinary instance: it can be
/// deleted, and the installed kind is offered again.
#[tokio::test]
async fn an_instance_named_after_its_kind_can_be_deleted() -> Result<()> {
    let root = TestRoot::new()?;
    let app = root
        .start(ServerConfig::new().executable("pi", fakes::path(Fake::Acp)?))
        .await?;
    let created = app
        .post_expect(
            "/api/provider-instances",
            json!({"id": new_id(), "instanceId": "pi", "kind": "pi", "displayName": "Pi",
                "enabled": true}),
            201,
        )
        .await?;
    let detected = app
        .get_expect("/api/provider-instances/detected", 200)
        .await?;
    ensure!(
        row(&detected, "pi").is_err(),
        "offered while added: {detected}"
    );
    app.post_expect(
        "/api/provider-instances/pi/delete",
        json!({"id": new_id(), "instanceId": "pi", "revision": created["revision"]}),
        200,
    )
    .await?;
    let list = app.get("/api/provider-instances").await?;
    ensure!(row(&list, "pi").is_err(), "not deleted: {list}");
    let detected = app
        .get_expect("/api/provider-instances/detected", 200)
        .await?;
    ensure!(
        row(&detected, "pi").is_ok(),
        "not offered again: {detected}"
    );
    Ok(())
}
