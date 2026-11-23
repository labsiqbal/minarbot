#![forbid(unsafe_code)]

mod agent_profile;
mod api;
mod appearance;
#[cfg(feature = "workflows")]
mod automation;
mod connected_apps;
mod connection_ui;
#[cfg(feature = "conversation")]
pub mod conversation;
mod craft_anything;
mod email;
mod engines;
mod general;
mod hub_connection;
#[cfg(any(feature = "conversation", feature = "workflows"))]
mod lazy_route;
mod mcp_connections;
mod memory;
mod messaging_connections;
mod onboarding;
mod pairing;
mod project_integrations;
mod projects;
mod providers;
mod routes;
#[cfg(any(feature = "conversation", feature = "workflows"))]
mod routine_label;
#[cfg(any(feature = "conversation", feature = "workflows"))]
#[path = "automation/select.rs"]
mod select;
mod settings;
mod settings_recovery;
mod shell;
#[path = "../../shared/skill_files.rs"]
mod skill_files;
mod skill_import;
mod skills;
mod system;
mod vault;
mod voice;
mod workspace;
mod workspace_route;

use leptos::{mount::mount_to, prelude::*};
use wasm_bindgen::{prelude::*, JsCast};
#[cfg(not(any(feature = "conversation", feature = "workflows")))]
use web_sys::HtmlDialogElement;
use web_sys::HtmlElement;

#[cfg(not(any(feature = "conversation", feature = "workflows")))]
#[wasm_bindgen]
pub fn mount_appearance() -> Result<(), JsValue> {
    let document = document();
    let element = |id| {
        document
            .get_element_by_id(id)
            .ok_or_else(|| JsValue::from_str("Appearance document is incomplete"))
    };
    let root = element("leptos-root")?.dyn_into::<HtmlElement>()?;
    if root.has_attribute("data-pairing") {
        let error = document
            .query_selector("#appearance-fallback [role=alert]")?
            .and_then(|alert| alert.text_content())
            .unwrap_or_default();
        mount_to(root, move || pairing::page(error)).forget();
        return Ok(());
    }
    let return_to = root
        .get_attribute("data-return-to")
        .unwrap_or_else(|| "/".into());
    let config: settings::Config = serde_json::from_str(
        &element("appearance-config")?
            .text_content()
            .unwrap_or_default(),
    )
    .map_err(|_| JsValue::from_str("Appearance configuration is invalid"))?;
    let icons = config.workspace_icons.clone();
    let error_icon = config.error_icon.clone();
    let path = window().location().pathname().unwrap_or_default();
    if let Some(route) = routes::SettingsRoute::from_path(&path) {
        let mount = mount_to(root, move || {
            let colors = StoredValue::new(config.colors.clone());
            provide_context(config.connection_icons.clone());
            provide_context(config.system_icons.clone());
            view! {
                <shell::WorkspaceShell base_path=return_to.clone() colors=colors icons=icons.clone()>
                    {settings::shell(config, return_to, route)}
                </shell::WorkspaceShell>
            }
        });
        element("appearance-dialog")?
            .dyn_into::<HtmlDialogElement>()?
            .show_modal()?;
        settings::focus_heading();
        mount.forget();
    } else if let Some(route) =
        workspace_route::WorkspaceRoute::from_path(&path).filter(|route| !route.is_automation())
    {
        mount_to(root, move || {
            let colors = StoredValue::new(config.colors);
            provide_context(config.connection_icons);
            provide_context(config.system_icons);
            view! {
                <shell::WorkspaceShell base_path=path colors=colors icons=icons>
                    {workspace::page(route, error_icon)}
                </shell::WorkspaceShell>
            }
        })
        .forget();
    } else {
        return Err(JsValue::from_str("Unknown Minarbot route"));
    }
    Ok(())
}

#[cfg(feature = "workflows")]
#[wasm_bindgen]
pub fn mount_workflows() -> Result<(), JsValue> {
    let document = document();
    let root = document
        .get_element_by_id("leptos-root")
        .ok_or_else(|| JsValue::from_str("Automation document is incomplete"))?
        .dyn_into::<HtmlElement>()?;
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Config {
        colors: Vec<appearance::ColorTheme>,
        workspace_icons: shell::Icons,
        connection_icons: connection_ui::Icons,
        system_icons: system::kit::Icons,
        error_icon: String,
    }
    let config = document
        .get_element_by_id("appearance-config")
        .and_then(|element| element.text_content())
        .and_then(|raw| serde_json::from_str::<Config>(&raw).ok())
        .ok_or_else(|| JsValue::from_str("Automation configuration is invalid"))?;
    let path = window().location().pathname()?;
    let route = workspace_route::WorkspaceRoute::from_path(&path)
        .ok_or_else(|| JsValue::from_str("Unknown automation route"))?;
    mount_to(root, move || {
        let colors = StoredValue::new(config.colors);
        provide_context(config.connection_icons);
        provide_context(config.system_icons);
        if route.is_automation() {
            automation::mount(path, colors, config.workspace_icons).into_any()
        } else {
            view! {
                <shell::WorkspaceShell base_path=path colors icons=config.workspace_icons>
                    {workspace::page(route, config.error_icon)}
                </shell::WorkspaceShell>
            }
            .into_any()
        }
    })
    .forget();
    Ok(())
}

#[cfg(any(feature = "conversation", feature = "workflows"))]
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RouteIcons {
    connection_icons: connection_ui::Icons,
    system_icons: system::kit::Icons,
}

#[cfg(any(feature = "conversation", feature = "workflows"))]
fn route_config<T: serde::de::DeserializeOwned>() -> Result<T, JsValue> {
    document()
        .get_element_by_id("appearance-config")
        .and_then(|element| element.text_content())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .ok_or_else(|| JsValue::from_str("Route configuration is invalid"))
}

#[cfg(feature = "workflows")]
#[wasm_bindgen]
pub fn mount_scoped_workflows(root: HtmlElement, path: &str) -> Result<JsValue, JsValue> {
    let (owner, back) = lazy_route::workflow_scope(path)
        .ok_or_else(|| JsValue::from_str("Unknown workflow owner"))?;
    let config: RouteIcons = route_config()?;
    let mount = mount_to(root, move || {
        provide_context(config.connection_icons);
        provide_context(config.system_icons);
        let directory = RwSignal::new(None::<Result<shell::Directory, String>>);
        provide_context(directory.read_only());
        leptos::task::spawn_local(async move {
            directory.try_set(Some(api::read("/api/agents").await));
        });
        automation::agent_library(owner, back)
    });
    Ok(wasm_bindgen::closure::Closure::once_into_js(move || {
        drop(mount)
    }))
}

#[cfg(feature = "conversation")]
#[wasm_bindgen]
pub fn mount_workflow_step(
    root: HtmlElement,
    agent: String,
    thread: String,
) -> Result<JsValue, JsValue> {
    if !workspace_route::valid_id(&agent) || !workspace_route::valid_id(&thread) {
        return Err(JsValue::from_str("Unknown step conversation"));
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Config {
        conversation_icons: conversation::Icons,
        #[serde(flatten)]
        icons: RouteIcons,
    }
    let config: Config = route_config()?;
    let mount = mount_to(root, move || {
        provide_context(config.conversation_icons);
        provide_context(config.icons.connection_icons);
        provide_context(config.icons.system_icons);
        conversation::workflow_step(agent, thread)
    });
    Ok(wasm_bindgen::closure::Closure::once_into_js(move || {
        drop(mount)
    }))
}

#[cfg(all(feature = "conversation", not(feature = "workflows")))]
#[wasm_bindgen]
pub fn mount_conversation() -> Result<(), JsValue> {
    let document = document();
    let root = document
        .get_element_by_id("leptos-root")
        .ok_or_else(|| JsValue::from_str("Conversation document is incomplete"))?
        .dyn_into::<HtmlElement>()?;
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Config {
        colors: Vec<appearance::ColorTheme>,
        workspace_icons: shell::Icons,
        conversation_icons: conversation::Icons,
        connection_icons: connection_ui::Icons,
        system_icons: system::kit::Icons,
    }
    let config = document
        .get_element_by_id("appearance-config")
        .and_then(|element| element.text_content())
        .and_then(|raw| serde_json::from_str::<Config>(&raw).ok())
        .ok_or_else(|| JsValue::from_str("Conversation configuration is invalid"))?;
    let path = window().location().pathname()?;
    if path == "/"
        && window()
            .location()
            .search()
            .ok()
            .and_then(|query| web_sys::UrlSearchParams::new_with_str(&query).ok())
            .is_some_and(|query| query.has("computer"))
    {
        mount_to(root, move || {
            appearance::initialize(StoredValue::new(config.colors));
            provide_context(config.conversation_icons);
            conversation::computer_window()
        })
        .forget();
        return Ok(());
    }
    mount_to(root, move || {
        let colors = StoredValue::new(config.colors);
        provide_context(config.conversation_icons);
        provide_context(config.connection_icons);
        provide_context(config.system_icons);
        let search = window().location().search().unwrap_or_default();
        provide_context(conversation::Navigation(RwSignal::new(format!("{path}{search}"))));
        provide_context(conversation::ProjectConversation(RwSignal::new(false)));
        view! {
            <shell::WorkspaceShell base_path=path colors=colors icons=config.workspace_icons conversation_navigation=true>
                {conversation::standalone()}
            </shell::WorkspaceShell>
        }
    }).forget();
    Ok(())
}
