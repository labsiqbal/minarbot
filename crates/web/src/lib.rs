#![forbid(unsafe_code)]

mod appearance;
pub mod conversation;
mod settings;

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
    let return_to = root
        .get_attribute("data-return-to")
        .unwrap_or_else(|| "/".into());
    let config = serde_json::from_str(
        &element("appearance-config")?
            .text_content()
            .unwrap_or_default(),
    )
    .map_err(|_| JsValue::from_str("Appearance configuration is invalid"))?;
    let mount = mount_to(root, move || settings::shell(config, return_to));
    element("appearance-dialog")?
        .dyn_into::<HtmlDialogElement>()?
        .show_modal()?;
    element("appearance-title")?
        .dyn_into::<HtmlElement>()?
        .focus()?;
    mount.forget();
    Ok(())
}

#[wasm_bindgen]
pub fn mount_conversation() -> Result<(), JsValue> {
    let document = document();
    let root = document
        .get_element_by_id("leptos-root")
        .ok_or_else(|| JsValue::from_str("Conversation document is incomplete"))?
        .dyn_into::<HtmlElement>()?;
    if let Some(config) = document
        .get_element_by_id("appearance-config")
        .and_then(|element| element.text_content())
        .and_then(|raw| serde_json::from_str::<settings::Config>(&raw).ok())
    {
        appearance::restore(&config.colors);
    }
    mount_to(root, conversation::standalone).forget();
    Ok(())
}
