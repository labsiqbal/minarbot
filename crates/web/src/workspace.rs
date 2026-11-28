#[cfg(feature = "workspace")]
use crate::{onboarding, projects, workspace_route::WorkspaceRoute};
use leptos::prelude::*;

/// First run at `/` while no agent can chat.
#[cfg(feature = "workspace")]
pub fn setup(error_icon: String) -> impl IntoView {
    document().set_title(&format!("{} · Minarbot", WorkspaceRoute::Setup.title()));
    onboarding::page(error_icon)
}

#[cfg(feature = "workspace")]
pub fn project(id: String, error_icon: String) -> impl IntoView {
    document().set_title(&format!(
        "{} · Minarbot",
        WorkspaceRoute::Project(String::new()).title()
    ));
    projects::page(id, error_icon)
}

pub fn heading(title: String, description: String) -> impl IntoView {
    view! { <header class="k-page-head">{heading_text(title, description)}</header> }
}

pub fn heading_text(title: String, description: String) -> impl IntoView {
    let heading = NodeRef::<leptos::html::H1>::new();
    heading.on_load(|heading| {
        // Preserve focus returned by Settings when a lazy route finishes mounting.
        if !document().active_element().is_some_and(|element| {
            element
                .matches(".sidebar-settings, .sidebar-reveal")
                .unwrap_or(false)
        }) {
            let _ = heading.focus();
        }
    });
    view! {
        <div class="k-page-head__text">
            <h1 node_ref=heading tabindex="-1">{title}</h1>
            {(!description.is_empty()).then(|| view! { <p>{description}</p> })}
        </div>
    }
}
