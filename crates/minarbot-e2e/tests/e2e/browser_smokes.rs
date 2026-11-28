//! UI smokes: the main journeys in headless Chrome against the built
//! Leptos client (`cargo xtask leptos`). Each smoke fails on a console
//! error, an uncaught exception or a failed request (see
//! `minarbot_testkit::browser`). Assertions use visible text, roles and
//! accessible names, never pixels or timings.
//!
//! Agent replies come from `fake-gateway`, the kit's Codex app-server peer,
//! which lists and answers with a `ScriptedModel`.

use minarbot_testkit::{
    browser::{self, Browser, Element},
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

#[tokio::test]
async fn providers_add_a_detected_cli_and_offer_it_again_after_delete() -> Result<()> {
    let root = TestRoot::new()?;
    let app = start_with_claude(&root).await?;
    browser::smoke("providers-detected", async |page: &Browser| {
        page.goto(&app.url("/settings/providers")).await?;
        page.find("heading", "Providers").await?;
        let machine = page.find("group", "On this machine").await?;
        page.click_element(page.find_in("button", "Add Claude", Some(machine)).await?)
            .await?;
        page.find("heading", "Claude").await?;
        page.gone("group", "On this machine").await?;
        // An instance named after its kind deletes like any other.
        page.click("button", "Delete instance").await?;
        let confirm = page
            .find("alertdialog", "Delete provider instance?")
            .await?;
        page.click_element(
            page.find_in("button", "Delete instance", Some(confirm))
                .await?,
        )
        .await?;
        page.gone("heading", "Claude").await?;
        let machine = page.find("group", "On this machine").await?;
        page.find_in("button", "Add Claude", Some(machine)).await?;
        Ok(())
    })
    .await
}

/// The agent profile lists the provider's models in a dropdown (a native
/// datalist opened off to the side, unstyled, with each ID twice).
#[tokio::test]
async fn agent_profile_lists_the_provider_models_in_a_dropdown() -> Result<()> {
    let root = TestRoot::new()?;
    let app = start(&root).await?;
    minarbot_testkit::providers::create_instance(
        &app,
        "work-hermes",
        "hermes",
        fakes::path(Fake::Acp)?,
    )
    .await?;
    let agent = minarbot_testkit::providers::AgentSpec::new("Model Agent")
        .field("model", "fixture-model")
        .instance("work-hermes")
        .create(&app)
        .await?;
    let agent = agent["id"].as_str().ok_or("agent id")?.to_owned();
    browser::smoke("agent-model", async |page: &Browser| {
        page.goto(&app.url(&format!("/agents/{agent}"))).await?;
        page.find("textbox", "Message Model Agent").await?;
        page.evaluate("document.querySelector('.chat__pill').click()")
            .await?;
        // The pill menu and the sidebar row menu both offer it.
        let edit = page.query("button", Some("Edit profile"), None).await?;
        page.click_element(edit.first().ok_or("no Edit profile")?.0)
            .await?;
        page.click("tab", "Model").await?;
        let labels = until_value(
            "model options",
            || async {
                page.evaluate(
                    "[...document.querySelectorAll('#model option')].map(o => o.textContent)",
                )
                .await
            },
            |labels| labels.to_string().contains("Fixture model"),
        )
        .await?;
        ensure!(
            labels == json!(["Choose a model", "Fixture model"]),
            "model options: {labels}"
        );
        let field = page.find("combobox", "Model").await?;
        let chosen = page.on(field, "function() { return this.value; }").await?;
        ensure!(chosen == "fixture-model", "selected model: {chosen}");
        // The provider list loads after the select; the bound instance stays chosen.
        let provider = page.find("combobox", "Provider instance").await?;
        let bound = page
            .on(provider, "function() { return this.value; }")
            .await?;
        ensure!(bound == "work-hermes", "provider: {bound}");
        let lists = page
            .evaluate("document.querySelectorAll('datalist').length")
            .await?;
        ensure!(lists == 0, "datalists left: {lists}");
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
        mark_document(page).await?;
        for name in ["Routines", "Workflows", "Automation calendar"] {
            page.click("button", name).await?;
            page.find("heading", name).await?;
            page.expect_clean(name)?;
            same_document(page, name).await?;
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
            same_document(page, section).await?;
        }
        // Settings closes onto the calendar it opened over.
        page.click("button", "Close settings").await?;
        page.gone("navigation", "Settings").await?;
        page.find("heading", "Automation calendar").await?;
        same_document(page, "Close settings").await?;
        page.evaluate("history.back()").await?;
        page.find("navigation", "Settings").await?;
        same_document(page, "history.back()").await?;
        page.expect_clean("history.back()")?;
        Ok(())
    })
    .await
}

/// Marks the loaded document so [`same_document`] can tell an in-app
/// route change from a page load.
async fn mark_document(page: &Browser) -> Result<()> {
    page.evaluate("window.__noReload = 1").await?;
    Ok(())
}

async fn same_document(page: &Browser, step: &str) -> Result<()> {
    let marked = page.evaluate("window.__noReload === 1").await?;
    ensure!(marked == json!(true), "{step} loaded a new document");
    Ok(())
}

#[tokio::test]
async fn workspace_routes_load_directly() -> Result<()> {
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
    let project = new_id();
    app.post_expect(
        "/api/projects",
        json!({"id": project, "name": "Smoke Project"}),
        200,
    )
    .await?;
    browser::smoke("direct-routes", async |page: &Browser| {
        page.goto(&app.url("/routines")).await?;
        page.find("heading", "Routines").await?;
        page.expect_clean("/routines")?;

        page.goto(&app.url(&format!("/projects/{project}?projectTab=calendar")))
            .await?;
        page.find("heading", "Smoke Project").await?;
        page.find("tabpanel", "Calendar").await?;
        page.expect_clean("project calendar")?;

        page.goto(&app.url(&format!("/workflows?agent={agent}&from=/agents/{agent}")))
            .await?;
        page.find("heading", "Workflows").await?;
        page.wait_text("Workflow library").await?;
        page.expect_clean("agent workflows")?;

        page.goto(&app.url("/settings/general")).await?;
        page.find("heading", "General").await?;
        page.expect_clean("/settings/general")?;
        mark_document(page).await?;
        page.click("button", "Close settings").await?;
        page.find("textbox", "Message Smoke Agent").await?;
        same_document(page, "Close settings").await?;
        page.expect_clean("chat after Settings")?;
        Ok(())
    })
    .await
}

/// The phone layout at 390 x 844: the agent list with bottom tabs, a chat
/// as its own page, the details sheet, the agent menu as a bottom sheet,
/// Automations and a Settings section, all in one document and none wider
/// than the screen. `minarbot:back` is the phone app's Back button and
/// `minarbot:open` its notification tap.
#[tokio::test]
async fn phone_layout_pages_sheets_and_back() -> Result<()> {
    let root = TestRoot::new()?;
    let model = catalog_model().await?;
    let app = start(&root).await?;
    seed_instance(&app, &root, &model).await?;
    minarbot_testkit::providers::AgentSpec::new("Smoke Agent")
        .field("model", MODELS[0])
        .instance("smoke-codex")
        .create(&app)
        .await?;
    browser::smoke("phone", async |page: &Browser| {
        page.phone(390, 844).await?;
        page.goto(&app.url("/")).await?;
        mark_document(page).await?;
        let agents = page.find("navigation", "Choose agent").await?;
        page.find("navigation", "Main").await?;
        fits(page, "agent list").await?;

        let rows = page.query("link", None, Some(agents)).await?;
        let (row, _) = rows
            .iter()
            .find(|(_, name)| name.contains("Smoke Agent"))
            .ok_or_else(|| format!("agent rows: {rows:?}"))?;
        page.click_element(*row).await?;
        page.find("textbox", "Message Smoke Agent").await?;
        page.gone("navigation", "Main").await?;
        page.find("button", "Back to agents").await?;
        fits(page, "chat").await?;

        page.click("button", "Show details").await?;
        let panel = page.find("dialog", "Conversation panel").await?;
        full_screen(page, panel, "details sheet").await?;
        page.click("button", "Close details").await?;
        page.gone("dialog", "Conversation panel").await?;

        page.evaluate("document.querySelector('.chat__pill').click()")
            .await?;
        let menu = page.find("navigation", "Menu Smoke Agent").await?;
        until_value(
            "agent menu as a bottom sheet",
            || async {
                page.on(
                    menu,
                    "function() { const box = this.getBoundingClientRect(); \
                     return [box.left, Math.round(box.right), Math.round(box.bottom)]; }",
                )
                .await
            },
            |sheet| sheet == &json!([0, 390, 844]),
        )
        .await?;
        ensure!(phone_back(page).await?, "Back did not close the agent menu");
        page.gone("navigation", "Menu Smoke Agent").await?;
        fits(page, "chat after the menu").await?;

        page.click("button", "Back to agents").await?;
        page.find("navigation", "Choose agent").await?;
        page.find("navigation", "Main").await?;

        // Settings over the agent list, then Automations from its tabs.
        page.click("link", "Settings").await?;
        let list = page.find("navigation", "Settings").await?;
        page.find_in("button", "General", Some(list)).await?;
        fits(page, "Settings over the agent list").await?;
        page.click("link", "Automations").await?;
        page.find("heading", "Routines").await?;
        page.find("navigation", "Automations").await?;
        fits(page, "Routines").await?;
        page.click("link", "Workflows").await?;
        page.find("heading", "Workflows").await?;
        fits(page, "Workflows").await?;

        page.click("link", "Settings").await?;
        let list = page.find("navigation", "Settings").await?;
        fits(page, "Settings list").await?;
        page.click_element(page.find_in("button", "General", Some(list)).await?)
            .await?;
        page.find("heading", "General").await?;
        page.gone("navigation", "Settings").await?;
        fits(page, "General").await?;
        page.click("button", "Settings").await?;
        page.find("navigation", "Settings").await?;

        // Back steps through the app's own history: the list came from Workflows.
        ensure!(phone_back(page).await?, "Back was not handled");
        page.gone("navigation", "Settings").await?;
        page.find("heading", "Workflows").await?;

        // A tapped notification opens its route in the app.
        let opened = page
            .evaluate(
                "!window.dispatchEvent(new CustomEvent('minarbot:open', { detail: '/routines', cancelable: true }))",
            )
            .await?;
        ensure!(opened == json!(true), "minarbot:open was not handled");
        page.find("heading", "Routines").await?;
        same_document(page, "phone journey").await?;
        page.expect_clean("phone journey")?;
        Ok(())
    })
    .await
}

/// On a wide screen both panes resize by pointer and keyboard, stay inside
/// their bounds and beside a 420 px chat, keep their widths across in-app
/// routes and a reload, and a double-click restores the default. Phones
/// show no handles.
#[tokio::test]
async fn sidebar_and_details_panel_resize_and_keep_their_widths() -> Result<()> {
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
    let chat = app.url(&format!("/agents/{agent}"));
    browser::smoke("resize", async |page: &Browser| {
        page.goto(&chat).await?;
        page.find("textbox", "Message Smoke Agent").await?;
        let sidebar = page.find("separator", "Resize sidebar").await?;
        let details = page.find("separator", "Resize details panel").await?;
        widths(page, [300, 360], "default").await?;
        ensure!(
            separator(page, sidebar).await? == json!(["220", "480", "300"]),
            "sidebar separator values: {}",
            separator(page, sidebar).await?
        );

        page.drag(sidebar, 80.0).await?;
        widths(page, [380, 360], "sidebar dragged").await?;
        page.drag(details, -100.0).await?;
        widths(page, [380, 460], "panel dragged").await?;
        page.expect_clean("drag")?;

        // 1280 px minus the 460 px panel and the 420 px chat leaves 400.
        page.on(sidebar, "function() { this.focus(); }").await?;
        page.key("ArrowLeft").await?;
        widths(page, [364, 460], "ArrowLeft").await?;
        page.key("Shift+ArrowLeft").await?;
        widths(page, [300, 460], "Shift+ArrowLeft").await?;
        page.key("Home").await?;
        widths(page, [220, 460], "Home").await?;
        page.key("End").await?;
        widths(page, [400, 460], "End").await?;
        page.key("ArrowLeft").await?;
        page.on(details, "function() { this.focus(); }").await?;
        page.key("ArrowLeft").await?;
        widths(page, [384, 476], "panel ArrowLeft").await?;
        page.key("End").await?;
        widths(page, [384, 476], "panel End beside the sidebar").await?;
        page.key("ArrowRight").await?;
        widths(page, [384, 460], "panel ArrowRight").await?;

        mark_document(page).await?;
        page.click("button", "Routines").await?;
        page.find("heading", "Routines").await?;
        until_value(
            "sidebar width on Routines",
            || async { pane_width(page, ".leptos-workspace > .sidebar").await },
            |width| width == &json!(384),
        )
        .await?;
        page.evaluate("history.back()").await?;
        page.find("textbox", "Message Smoke Agent").await?;
        widths(page, [384, 460], "back in the chat").await?;
        same_document(page, "resize routes").await?;

        page.reload().await?;
        page.find("textbox", "Message Smoke Agent").await?;
        widths(page, [384, 460], "after reload").await?;
        let sidebar = page.find("separator", "Resize sidebar").await?;
        ensure!(
            separator(page, sidebar).await? == json!(["220", "400", "384"]),
            "restored sidebar separator values: {}",
            separator(page, sidebar).await?
        );

        page.double_click(sidebar).await?;
        widths(page, [300, 460], "sidebar double-click").await?;
        page.double_click(page.find("separator", "Resize details panel").await?)
            .await?;
        page.reload().await?;
        page.find("textbox", "Message Smoke Agent").await?;
        widths(page, [300, 360], "defaults after reload").await?;
        page.expect_clean("resize journey")?;
        Ok(())
    })
    .await?;
    browser::smoke("resize-phone", async |page: &Browser| {
        page.phone(390, 844).await?;
        page.goto(&chat).await?;
        page.evaluate(
            "localStorage.setItem('minarbot:sidebar-width', '480'); \
             localStorage.setItem('minarbot:details-width', '600')",
        )
        .await?;
        page.reload().await?;
        page.find("textbox", "Message Smoke Agent").await?;
        page.click("button", "Show details").await?;
        let panel = page.find("dialog", "Conversation panel").await?;
        full_screen(page, panel, "details sheet with stored widths").await?;
        let handles = page.query("separator", None, None).await?;
        ensure!(
            !handles.iter().any(|(_, name)| name.starts_with("Resize")),
            "phone shows resize handles: {handles:?}"
        );
        page.click("button", "Close details").await?;
        page.click("button", "Back to agents").await?;
        page.find("navigation", "Choose agent").await?;
        fits(page, "agent list with stored widths").await?;
        let handles = page.query("separator", None, None).await?;
        ensure!(
            !handles.iter().any(|(_, name)| name.starts_with("Resize")),
            "phone list shows resize handles: {handles:?}"
        );
        Ok(())
    })
    .await
}

/// The rendered width of the first element matching `selector`, rounded.
async fn pane_width(page: &Browser, selector: &str) -> Result<Value> {
    page.evaluate(&format!(
        "(e => e ? Math.round(e.getBoundingClientRect().width) : null)(document.querySelector({}))",
        json!(selector)
    ))
    .await
}

/// Wait until the sidebar and the open details panel are `want` wide.
async fn widths(page: &Browser, want: [u64; 2], step: &str) -> Result<()> {
    until_value(
        step,
        || async {
            Ok(json!([
                pane_width(page, ".leptos-workspace > .sidebar").await?,
                pane_width(page, ".conversation-panel-shell[open]").await?,
            ]))
        },
        |widths| widths == &json!(want),
    )
    .await?;
    Ok(())
}

/// A separator's `aria-valuemin`, `aria-valuemax` and `aria-valuenow`.
async fn separator(page: &Browser, element: Element) -> Result<Value> {
    page.on(
        element,
        "function() { return ['aria-valuemin', 'aria-valuemax', 'aria-valuenow'].map(n => this.getAttribute(n)); }",
    )
    .await
}

/// No page wider than the screen: neither the document nor its main panes.
async fn fits(page: &Browser, step: &str) -> Result<()> {
    let overflow = page
        .evaluate(
            "[document.scrollingElement, ...document.querySelectorAll('.sidebar, .leptos-workspace__outlet, .chat, .transcript, .app-settings__nav, .app-settings__main')]
                .filter(e => e.offsetParent !== null || e === document.scrollingElement)
                .filter(e => e.scrollWidth > Math.max(e.clientWidth, 0) + 1 || e.getBoundingClientRect().right > innerWidth + 1)
                .map(e => `${e.className || e.tagName} ${e.scrollWidth}/${e.clientWidth}`)",
        )
        .await?;
    ensure!(
        overflow == json!([]),
        "{step} is wider than the screen: {overflow}"
    );
    Ok(())
}

/// The sheet covers the 390 x 844 screen once it has slid in.
async fn full_screen(page: &Browser, element: Element, step: &str) -> Result<()> {
    until_value(
        step,
        || async {
            page.on(
                element,
                "function() { const box = this.getBoundingClientRect(); \
                 return [box.left, box.top, Math.round(box.right), Math.round(box.bottom)]; }",
            )
            .await
        },
        |size| size == &json!([0, 0, 390, 844]),
    )
    .await?;
    Ok(())
}

/// The phone app's Back button: true when the web client handled it.
async fn phone_back(page: &Browser) -> Result<bool> {
    Ok(page
        .evaluate("!window.dispatchEvent(new CustomEvent('minarbot:back', { cancelable: true }))")
        .await?
        == json!(true))
}
