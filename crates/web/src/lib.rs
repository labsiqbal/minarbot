#![forbid(unsafe_code)]

mod api;
mod appearance;
#[cfg(feature = "conversation")]
pub mod conversation;
mod general;
mod onboarding;
mod pairing;
mod projects;
mod routes;
mod settings;
mod shell;
mod voice;
mod workspace;
mod workspace_route;

use leptos::{mount::mount_to, prelude::*};
use wasm_bindgen::{prelude::*, JsCast};
#[cfg(not(feature = "conversation"))]
use web_sys::HtmlDialogElement;
use web_sys::HtmlElement;

#[cfg(not(feature = "conversation"))]
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
    } else if let Some(route) = workspace_route::WorkspaceRoute::from_path(&path) {
        mount_to(root, move || {
            let colors = StoredValue::new(config.colors);
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

#[cfg(feature = "conversation")]
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
    }
    let config = document
        .get_element_by_id("appearance-config")
        .and_then(|element| element.text_content())
        .and_then(|raw| serde_json::from_str::<Config>(&raw).ok())
        .ok_or_else(|| JsValue::from_str("Conversation configuration is invalid"))?;
    let path = window().location().pathname()?;
    mount_to(root, move || {
        let colors = StoredValue::new(config.colors);
        provide_context(config.conversation_icons);
        view! {
            <shell::WorkspaceShell base_path=path colors=colors icons=config.workspace_icons conversation_navigation=true>
                {conversation::standalone()}
            </shell::WorkspaceShell>
        }
    }).forget();
    Ok(())
}
