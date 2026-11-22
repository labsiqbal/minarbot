#![forbid(unsafe_code)]

mod api;
mod appearance;
mod general;
mod pairing;
mod routes;
mod settings;
mod shell;
mod voice;

use leptos::{mount::mount_to, prelude::*};
use wasm_bindgen::{prelude::*, JsCast};
use web_sys::{HtmlDialogElement, HtmlElement};

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
    let path = window().location().pathname().unwrap_or_default();
    let route = routes::SettingsRoute::from_path(&path)
        .ok_or_else(|| JsValue::from_str("Unknown settings route"))?;
    let mount = mount_to(root, move || {
        let colors = StoredValue::new(config.colors.clone());
        view! {
            <shell::WorkspaceShell base_path=return_to.clone() colors=colors icons=config.workspace_icons.clone()>
                {settings::shell(config, return_to, route)}
            </shell::WorkspaceShell>
        }
    });
    element("appearance-dialog")?
        .dyn_into::<HtmlDialogElement>()?
        .show_modal()?;
    settings::focus_heading();
    mount.forget();
    Ok(())
}
