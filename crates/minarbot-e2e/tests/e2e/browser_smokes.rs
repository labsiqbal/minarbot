//! UI smokes: the main journeys in headless Chrome against the built
//! Leptos client (`cargo xtask leptos`). Each smoke fails on a console
//! error, an uncaught exception or a failed request (see
//! `minarbot_testkit::browser`). Assertions use visible text, roles and
//! accessible names, never pixels or timings.
//!
//! Agent replies come from `fake-gateway`, the kit's Codex app-server peer,
//! which lists and answers with a `ScriptedModel`.

use minarbot_testkit::{
    browser::{self, Browser},
    fake::native::{CLAUDE_MODELS, SIGN_IN_CODE},
    fakes::{self, Fake},
    model::{Reply, ScriptedModel},
    prelude::*,
};

const MODELS: [&str; 2] = ["smoke-fast", "smoke-deep"];

async fn start(root: &TestRoot) -> Result<Server> {
    let dist = browser::client_dist()?.to_string_lossy().into_owned();
    root.start(ServerConfig::new().option("dist", dist)).await
}

/// A scripted model that lists [`MODELS`].
async fn catalog_model() -> Result<ScriptedModel> {
    let model = ScriptedModel::start().await?;
    model.catalog(MODELS);
    Ok(model)
}

/// A signed-in `CODEX_HOME`: the server reports a Codex instance as
/// authenticated when `auth.json` exists there.
fn codex_home(root: &TestRoot) -> Result<String> {
    let home = root.dir("codex-home")?;
    std::fs::write(home.join("auth.json"), "{}")?;
    Ok(home.to_string_lossy().into_owned())
}

/// The Codex instance `smoke-codex`, created through the API, running
/// `fake-gateway` against `model` with streaming on.
async fn seed_instance(app: &Server, root: &TestRoot, model: &ScriptedModel) -> Result<()> {
    app.post_expect(
        "/api/provider-instances",
        json!({"id": new_id(), "instanceId": "smoke-codex", "kind": "codex",
        "displayName": "Smoke Codex", "enabled": true,
        "binaryPath": fakes::path(Fake::Gateway)?, "homePath": codex_home(root)?,
        "environment": [
            {"name": "MINARBOT_GATEWAY_BASE_URL", "value": model.base_url(), "secret": false},
            {"name": "MINARBOT_GATEWAY_STREAM", "value": "true", "secret": false},
        ]}),
        201,
    )
    .await?;
    Ok(())
}

/// A server whose host has the fake Claude Code installed (signed out).
async fn start_with_claude(root: &TestRoot) -> Result<Server> {
    let dist = browser::client_dist()?.to_string_lossy().into_owned();
    root.start(
        ServerConfig::new()
            .option("dist", dist)
            .executable("claude", fakes::path(Fake::Claude)?),
    )
    .await
}

/// Paste the fake Claude code into the open sign-in dialog `title`.
async fn finish_claude_sign_in(page: &Browser, title: &str) -> Result<()> {
    let dialog = page.find("dialog", title).await?;
    let open = page.find_in("link", "Open", Some(dialog)).await?;
    let href = page.on(open, "function() { return this.href; }").await?;
    ensure!(
        href == "https://claude.ai/oauth/authorize?code=true&state=fixture",
        "sign-in link: {href}"
    );
    page.fill("textbox", "Paste the code it shows", SIGN_IN_CODE)
        .await?;
    page.click_element(page.find_in("button", "Sign in", Some(dialog)).await?)
        .await?;
    page.gone("dialog", title).await
}

async fn checked(page: &Browser, radio: &str) -> Result<bool> {
    let element = page.find("radio", radio).await?;
    Ok(page
        .on(
            element,
            "function() { return this.getAttribute('aria-checked'); }",
        )
        .await?
        == "true")
}

#[tokio::test]
async fn setup_signs_in_a_detected_provider_and_opens_the_agent_form() -> Result<()> {
    let root = TestRoot::new()?;
    let app = start_with_claude(&root).await?;
    browser::smoke("setup-sign-in", async |page: &Browser| {
        page.goto(&app.url("/")).await?;
        page.find("heading", "Set up Minarbot").await?;
        page.wait_text("Connect a provider").await?;
        // Claude is offered as soon as it is installed, before any setup.
        page.find("radio", "Claude").await?;
        page.wait_text("Not signed in").await?;
        page.click("button", "Sign in").await?;
        finish_claude_sign_in(page, "Sign in to Claude").await?;
        page.wait_text("Signed in").await?;
        until("Claude chosen", || async {
            Ok(checked(page, "Claude").await?.then_some(()))
        })
        .await?;
        // The model list comes from the signed-in Claude.
        let offered = wait_models(page).await?;
        ensure!(offered == CLAUDE_MODELS, "offered models: {offered:?}");
        page.fill("textbox", "Name", "Setup Agent").await?;
        page.click("button", "Create agent").await?;
        until("agent page", || async {
            Ok(page.url().await?.contains("/agents/").then_some(()))
        })
        .await?;
        page.find("textbox", "Message Setup Agent").await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn setup_adds_a_provider_in_the_wizard_and_signs_it_in() -> Result<()> {
    let root = TestRoot::new()?;
    let app = start_with_claude(&root).await?;
    browser::smoke("setup-add-provider", async |page: &Browser| {
        page.goto(&app.url("/")).await?;
        page.find("heading", "Set up Minarbot").await?;
        page.click("button", "Add provider").await?;
        let dialog = page.find("dialog", "Add provider").await?;
        page.click_element(page.find_in("button", "Claude", Some(dialog)).await?)
            .await?;
        page.click_element(page.find_in("button", "Continue", Some(dialog)).await?)
            .await?;
        page.fill("textbox", "Display name", "Work Claude").await?;
        page.click("button", "Sign in with Claude").await?;
        page.gone("dialog", "Add provider").await?;
        finish_claude_sign_in(page, "Sign in to Work Claude").await?;
        until("Work Claude chosen", || async {
            Ok(checked(page, "Work Claude").await?.then_some(()))
        })
        .await?;
        page.find("textbox", "Name").await?;
        Ok(())
    })
    .await
}

/// An agent creation that fails keeps its form open with the error and
/// the retry (the form once hid as soon as creation started).
#[tokio::test]
async fn setup_shows_a_failed_agent_creation_and_its_retry() -> Result<()> {
    let root = TestRoot::new()?;
    let app = start(&root).await?;
    browser::smoke("setup-retry", async |page: &Browser| {
        page.goto(&app.url("/")).await?;
        page.find("heading", "Set up Minarbot").await?;
        let command = json!({"id": new_id(), "name": "Lost Agent", "role": "", "model": "fixture",
            "instructions": "Help.", "threadLimit": 2, "toolBudget": 90,
            "providerInstance": "missing-instance", "modelOptions": {}});
        page.evaluate(&format!(
            "sessionStorage.setItem('minarbot-agent-creation', {})",
            json!(command.to_string())
        ))
        .await?;
        page.reload().await?;
        // The provider step loads after the form; click once it settled.
        page.find("button", "Add provider").await?;
        page.click("button", "Retry creation").await?;
        page.wait_text("Provider instance not found.").await?;
        page.find("button", "Retry creation").await?;
        let failed = page.take_problems();
        ensure!(
            failed == ["HTTP 400: POST ".to_owned() + &app.url("/api/agents")],
            "problems: {failed:?}"
        );
        Ok(())
    })
    .await
}

#[tokio::test]
async fn providers_add_a_codex_instance_and_offer_its_models() -> Result<()> {
    let root = TestRoot::new()?;
    let model = catalog_model().await?;
    let app = start(&root).await?;
    let gateway = fakes::path(Fake::Gateway)?;
    let home = codex_home(&root)?;
    browser::smoke("providers", async |page: &Browser| {
        page.goto(&app.url("/settings/providers")).await?;
        page.find("heading", "Providers").await?;
        page.click("button", "Add provider").await?;
        let dialog = page.find("dialog", "Add provider").await?;
        page.click_element(page.find_in("button", "Codex", Some(dialog)).await?)
            .await?;
        page.click_element(page.find_in("button", "Continue", Some(dialog)).await?)
            .await?;
        page.fill("textbox", "Display name", "Smoke Codex").await?;
        page.click("button", "Configure manually").await?;
        page.gone("dialog", "Add provider").await?;
        page.find("heading", "Smoke Codex").await?;
        page.fill("textbox", "Binary path", &gateway.to_string_lossy())
            .await?;
        page.fill("textbox", "CODEX_HOME path", &home).await?;
        for (index, (name, value)) in [
            ("MINARBOT_GATEWAY_BASE_URL", model.base_url()),
            ("MINARBOT_GATEWAY_STREAM", "true".to_owned()),
        ]
        .into_iter()
        .enumerate()
        {
            page.click("button", "Add variable").await?;
            let names = page.query("textbox", Some("Variable name"), None).await?;
            let (row, _) = names.get(index).ok_or("new variable row missing")?;
            page.fill_element(*row, name).await?;
            page.fill("textbox", &format!("Value for {name}"), &value)
                .await?;
        }
        page.click("button", "Save changes").await?;
        page.wait_text("Saved").await?;
        // The list row and the detail both show the signed-in home.
        page.wait_no_text("Not installed").await?;
        page.wait_text("Authenticated · ChatGPT").await?;
        let saved = app.get("/api/provider-instances").await?;
        let instance = saved["providers"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["displayName"] == "Smoke Codex"))
            .ok_or_else(|| format!("instance not saved: {saved}"))?;
        ensure!(
            instance["binaryPath"] == gateway.to_string_lossy().as_ref()
                && instance["environment"][0]["value"] == model.base_url(),
            "saved instance: {instance}"
        );

        // Setup now offers the instance with the models it lists.
        page.goto(&app.url("/")).await?;
        page.find("heading", "Set up Minarbot").await?;
        page.wait_text("Smoke Codex").await?;
        let offered = wait_models(page).await?;
        ensure!(offered == MODELS, "offered models: {offered:?}");
        page.select("Model", MODELS[1]).await?;
        let field = page.find("combobox", "Model").await?;
        let picked = page.on(field, "function() { return this.value; }").await?;
        ensure!(picked == MODELS[1], "picked model: {picked}");
        Ok(())
    })
    .await
}

/// The setup form's model suggestions once the instance's list arrived.
async fn wait_models(page: &Browser) -> Result<Vec<String>> {
    until_value(
        "onboarding model list",
        || async {
            let options = page
                .evaluate(
                    "[...document.querySelectorAll('select[aria-label=Model] option')].map(o => o.value).filter(Boolean)",
                )
                .await?;
            Ok(options
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>())
        },
        |models| !models.is_empty(),
    )
    .await
}

#[tokio::test]
async fn chat_creates_an_agent_streams_a_reply_and_keeps_history() -> Result<()> {
    let root = TestRoot::new()?;
    let model = catalog_model().await?;
    model.respond(|_| Reply::deltas(["Pong ", "from the ", "smoke model."]));
    let app = start(&root).await?;
    seed_instance(&app, &root, &model).await?;
    browser::smoke("chat", async |page: &Browser| {
        page.goto(&app.url("/")).await?;
        page.find("heading", "Set up Minarbot").await?;
        wait_models(page).await?;
        page.fill("textbox", "Name", "Smoke Agent").await?;
        page.click("button", "Create agent").await?;
        until("agent page", || async {
            Ok(page.url().await?.contains("/agents/").then_some(()))
        })
        .await?;
        let composer = page.find("textbox", "Message Smoke Agent").await?;
        page.fill_element(composer, "Ping from the smoke").await?;
        page.key("Enter").await?;
        page.wait_text("Pong from the smoke model.").await?;
        let asked = model.requests();
        ensure!(
            asked.len() == 1 && asked[0].last_user_text().contains("Ping from the smoke"),
            "model requests: {:?}",
            asked
                .iter()
                .map(|request| request.last_user_text())
                .collect::<Vec<_>>()
        );
        page.reload().await?;
        page.wait_text("Ping from the smoke").await?;
        page.wait_text("Pong from the smoke model.").await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn main_routes_render_without_errors() -> Result<()> {
    let root = TestRoot::new()?;
    let model = catalog_model().await?;
    let app = start(&root).await?;
    seed_instance(&app, &root, &model).await?;
    let agent = minarbot_testkit::providers::AgentSpec::new("Smoke Agent")
        .field("model", MODELS[0])
        .instance("smoke-codex")
        .create(&app)
        .await?;
    let agent = agent["id"].as_str().ok_or("agent id")?.to_owned();
    browser::smoke("routes", async |page: &Browser| {
        page.goto(&app.url(&format!("/agents/{agent}"))).await?;
        page.find("textbox", "Message Smoke Agent").await?;
        for name in ["Routines", "Workflows", "Automation calendar"] {
            page.click("button", name).await?;
            page.find("heading", name).await?;
            page.expect_clean(name)?;
        }
        page.click("link", "Settings").await?;
        let nav = page.find("navigation", "Settings").await?;
        let sections = page.query("button", None, Some(nav)).await?;
        ensure!(sections.len() >= 10, "settings sections: {sections:?}");
        for (_, section) in &sections {
            let button = page.find_in("button", section, Some(nav)).await?;
            page.click_element(button).await?;
            page.find("heading", section).await?;
            page.expect_clean(section)?;
        }
        Ok(())
    })
    .await
}
