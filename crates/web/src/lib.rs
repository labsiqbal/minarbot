#![forbid(unsafe_code)]
// Hosted Hub links shared modules and uses only part of each.
#![cfg_attr(feature = "hub", allow(dead_code))]

mod agent_profile;
mod api;
#[cfg(feature = "workspace")]
mod app;
mod appearance;
#[cfg(feature = "workspace")]
mod automation;
mod connected_apps;
mod connection_ui;
#[cfg(feature = "workspace")]
pub mod conversation;
mod email;
mod general;
#[cfg(feature = "hub")]
mod hub;
mod hub_connection;
#[cfg(not(feature = "hub"))]
mod lazy_view;
mod mcp_connections;
#[cfg(not(feature = "hub"))]
mod memory;
mod messaging_connections;
mod onboarding;
mod pairing;
mod project_integrations;
mod projects;
mod provider_icons;
mod provider_instances;
mod provider_sign_in;
mod router;
mod routes;
#[cfg(feature = "workspace")]
mod routine_label;
#[cfg(feature = "workspace")]
#[path = "automation/select.rs"]
mod select;
mod session;
mod settings;
mod settings_recovery;
mod shell;
#[path = "../../shared/skill_files.rs"]
mod skill_files;
mod skill_import;
mod skills;
mod system;
mod time_zone;
mod vault;
mod voice;
mod workspace;
mod workspace_route;

#[cfg(feature = "workspace")]
pub use app::mount_app;

#[cfg(feature = "hub")]
use leptos::{mount::mount_to, prelude::*};
#[cfg(feature = "hub")]
use wasm_bindgen::{prelude::*, JsCast};
#[cfg(feature = "hub")]
use web_sys::HtmlElement;

#[cfg(feature = "hub")]
#[wasm_bindgen]
pub fn mount_hub() -> Result<(), JsValue> {
    let root = document()
        .get_element_by_id("leptos-root")
        .ok_or_else(|| JsValue::from_str("Hub document is incomplete"))?
        .dyn_into::<HtmlElement>()?;
    let read = |id| {
        document()
            .get_element_by_id(id)
            .and_then(|node| node.text_content())
            .and_then(|raw| {
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&raw).ok()
            })
            .ok_or_else(|| JsValue::from_str("Hub configuration is invalid"))
    };
    let mut config = read("hub-presentation")?;
    config.extend(read("hub-config")?);
    let config = serde_json::from_value(serde_json::Value::Object(config))
        .map_err(|_| JsValue::from_str("Hub configuration is invalid"))?;
    mount_to(root, move || hub::page(config)).forget();
    Ok(())
}
