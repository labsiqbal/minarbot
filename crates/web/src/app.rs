// The workspace app: one document, one shell, one lazy chunk per route.
use crate::{
    appearance, connection_ui, lazy_view,
    router::{self, RouteKind, Router},
    routes::SettingsRoute,
    shell, system,
};
use leptos::{mount::mount_to, prelude::*};
use serde::{de::DeserializeOwned, Deserialize};
use std::cell::RefCell;
use wasm_bindgen::{prelude::*, JsCast};
use web_sys::HtmlElement;

thread_local! {
    static CONFIG: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Reads this document's configuration into the part a route needs.
/// Unknown fields belong to other routes.
pub(crate) fn config<T: DeserializeOwned>() -> Result<T, JsValue> {
    CONFIG
        .with_borrow(|raw| serde_json::from_str(raw))
        .map_err(|_| JsValue::from_str("Minarbot configuration is invalid"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShellConfig {
    colors: Vec<appearance::ColorTheme>,
    workspace_icons: shell::Icons,
    connection_icons: connection_ui::Icons,
    system_icons: system::kit::Icons,
    error_icon: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConversationIcons {
    conversation_icons: crate::conversation::Icons,
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = minarbotLoadConversation)]
    fn load_conversation_adapter() -> js_sys::Promise;
}

/// The browser adapter conversation views call, loaded once.
async fn conversation_adapter() -> Result<(), JsValue> {
    wasm_bindgen_futures::JsFuture::from(load_conversation_adapter())
        .await
        .map(|_| ())
}

fn provide_conversation_icons() -> Result<(), JsValue> {
    let icons: ConversationIcons = config()?;
    provide_context(icons.conversation_icons);
    Ok(())
}

#[wasm_bindgen]
pub fn mount_app() -> Result<(), JsValue> {
    let document = document();
    let root = document
        .get_element_by_id("leptos-root")
        .ok_or_else(|| JsValue::from_str("Minarbot document is incomplete"))?
        .dyn_into::<HtmlElement>()?;
    if root.has_attribute("data-pairing") {
        let error = document
            .query_selector("#appearance-fallback [role=alert]")?
            .and_then(|alert| alert.text_content())
            .unwrap_or_default();
        mount_to(root, move || {
            lazy_view::load("pairing", move || pairing(error.clone()))
        })
        .forget();
        return Ok(());
    }
    let raw = document
        .get_element_by_id("appearance-config")
        .and_then(|element| element.text_content())
        .unwrap_or_default();
    CONFIG.with_borrow_mut(|config| *config = raw);
    let config: ShellConfig = config()?;
    let colors = StoredValue::new(config.colors);
    let location = router::current();
    if RouteKind::parse(&location).is_none() {
        if !location.starts_with("/?") {
            return Err(JsValue::from_str("Unknown Minarbot route"));
        }
        // `/?computer=<agent>`: the Computer window opened from a chat.
        mount_to(root, move || {
            appearance::initialize(colors);
            lazy_view::load("Computer", computer)
        })
        .forget();
        return Ok(());
    }
    let router = Router::install(root.get_attribute("data-initial").as_deref() == Some("setup"));
    let icons = config.workspace_icons;
    let error_icon = StoredValue::new(config.error_icon);
    mount_to(root, move || {
        provide_context(config.connection_icons);
        provide_context(config.system_icons);
        provide_context(crate::conversation::ProjectConversation(RwSignal::new(
            false,
        )));
        view! {
            <shell::WorkspaceShell colors=colors icons=icons>
                {outlet(router, error_icon)}
            </shell::WorkspaceShell>
        }
    })
    .forget();
    Ok(())
}

/// The route under the shell, and Settings over it.
fn outlet(router: Router, error_icon: StoredValue<String>) -> impl IntoView {
    let directory = use_context::<ReadSignal<Option<Result<shell::Directory, String>>>>();
    if let Some(directory) = directory {
        Effect::new(move |_| {
            directory.with(|data| {
                if let Some(Ok(directory)) = data {
                    router.set_setup(
                        !directory
                            .agents
                            .iter()
                            .any(|agent| agent.runnable && !agent.hidden),
                    );
                }
            });
        });
    }
    let page = Memo::new(move |_| router.page().and_then(|url| router.kind_of(&url)));
    let settings = Memo::new(move |_| match router.kind() {
        Some(RouteKind::Settings(section)) => Some(section),
        _ => None,
    });
    Effect::new(move |_| {
        let conversation = page.with(|kind| kind.as_ref().is_some_and(RouteKind::is_conversation));
        if let Some(body) = document().body() {
            let _ = if conversation {
                body.set_attribute("data-view", "conversation")
            } else {
                body.remove_attribute("data-view")
            };
        }
    });
    view! {
        {move || page.get().map(|kind| match kind {
            // Inbox and remote agents carry their path, so each one mounts anew.
            RouteKind::Conversation | RouteKind::Root | RouteKind::Inbox(_) | RouteKind::Remote(..) => {
                lazy_view::load("conversation", conversation).into_any()
            }
            RouteKind::Automation => lazy_view::load("automation", automation).into_any(),
            RouteKind::Setup => {
                lazy_view::load("setup", move || setup(error_icon.get_value())).into_any()
            }
            RouteKind::Project(id) => {
                lazy_view::load("project", move || project(id.clone(), error_icon.get_value()))
                    .into_any()
            }
            // The shell shows the agent list itself.
            RouteKind::Settings(_) | RouteKind::AgentList => ().into_any(),
        })}
        <Show when=move || settings.with(Option::is_some)>
            {lazy_view::load("Settings", move || settings_overlay(settings))}
        </Show>
    }
}

#[leptos::wasm_split::wasm_split(app_pairing, wasm_split_path = leptos::wasm_split, fallible)]
fn pairing(error: String) -> lazy_view::ChunkView {
    Ok(crate::pairing::page(error).into_any())
}

async fn computer() -> Result<AnyView, JsValue> {
    conversation_adapter().await?;
    computer_chunk()
        .await
        .map_err(|_| JsValue::from_str("Computer could not load"))
}

#[leptos::wasm_split::wasm_split(app_computer, wasm_split_path = leptos::wasm_split, fallible)]
fn computer_chunk() -> lazy_view::ChunkView {
    provide_conversation_icons().ok();
    Ok(crate::conversation::computer_window().into_any())
}

async fn conversation() -> Result<AnyView, JsValue> {
    conversation_adapter().await?;
    conversation_chunk()
        .await
        .map_err(|_| JsValue::from_str("Conversation could not load"))
}

#[leptos::wasm_split::wasm_split(app_conversation, wasm_split_path = leptos::wasm_split, fallible)]
fn conversation_chunk() -> lazy_view::ChunkView {
    provide_conversation_icons().ok();
    Ok(crate::conversation::standalone().into_any())
}

#[leptos::wasm_split::wasm_split(app_automation, wasm_split_path = leptos::wasm_split, fallible)]
fn automation() -> lazy_view::ChunkView {
    Ok(crate::automation::outlet().into_any())
}

#[leptos::wasm_split::wasm_split(app_setup, wasm_split_path = leptos::wasm_split, fallible)]
fn setup(error_icon: String) -> lazy_view::ChunkView {
    Ok(crate::workspace::setup(error_icon).into_any())
}

#[leptos::wasm_split::wasm_split(app_project, wasm_split_path = leptos::wasm_split, fallible)]
fn project(id: String, error_icon: String) -> lazy_view::ChunkView {
    Ok(crate::workspace::project(id, error_icon).into_any())
}

async fn settings_overlay(section: Memo<Option<SettingsRoute>>) -> Result<AnyView, JsValue> {
    let config = config::<crate::settings::Config>()?;
    settings_chunk(config, section)
        .await
        .map_err(|_| JsValue::from_str("Settings could not load"))
}

#[leptos::wasm_split::wasm_split(app_settings, wasm_split_path = leptos::wasm_split, fallible)]
fn settings_chunk(
    config: crate::settings::Config,
    section: Memo<Option<SettingsRoute>>,
) -> lazy_view::ChunkView {
    Ok(crate::settings::overlay(config, section).into_any())
}

/// Agent-scoped workflows over a chat (`/workflows?agent=`).
pub(crate) fn scoped_workflows(owner: String, back: String) -> impl IntoView {
    lazy_view::load("workflows", move || {
        let library = crate::automation::take_library(&owner);
        scoped_workflows_chunk(owner.clone(), back.clone(), library)
    })
}

#[leptos::wasm_split::wasm_split(app_scoped_workflows, wasm_split_path = leptos::wasm_split, fallible)]
fn scoped_workflows_chunk(
    owner: String,
    back: String,
    library: Option<js_sys::Promise>,
) -> lazy_view::ChunkView {
    Ok(crate::automation::agent_library(owner, back, library).into_any())
}

/// Starts loading agent-scoped workflows while the chat menu opens.
pub(crate) fn preload_scoped_workflows() {
    leptos::task::spawn_local(async {
        let _ = __wasm_split_preload_scoped_workflows_chunk().await;
    });
}

/// The conversation of one workflow run step, inline in the run.
pub(crate) fn workflow_step(agent: String, thread: String) -> impl IntoView {
    lazy_view::load("step conversation", move || {
        let (agent, thread) = (agent.clone(), thread.clone());
        async move {
            conversation_adapter().await?;
            workflow_step_chunk(agent, thread)
                .await
                .map_err(|_| JsValue::from_str("Step conversation could not load"))
        }
    })
}

#[leptos::wasm_split::wasm_split(app_workflow_step, wasm_split_path = leptos::wasm_split, fallible)]
fn workflow_step_chunk(agent: String, thread: String) -> lazy_view::ChunkView {
    provide_conversation_icons().ok();
    Ok(crate::conversation::workflow_step(agent, thread).into_any())
}
