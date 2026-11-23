#[cfg(feature = "workflows")]
use crate::automation;
#[cfg(not(any(feature = "conversation", feature = "hub")))]
use crate::{onboarding, projects, workspace_route::WorkspaceRoute};
use leptos::prelude::*;

#[cfg(not(any(feature = "conversation", feature = "hub")))]
pub fn page(route: WorkspaceRoute, error_icon: String) -> impl IntoView {
    document().set_title(&format!("{} · Minarbot", route.title()));
    match route {
        WorkspaceRoute::Setup => onboarding::page(error_icon).into_any(),
        #[cfg(feature = "workflows")]
        WorkspaceRoute::Routines => automation::routines_page().into_any(),
        #[cfg(feature = "workflows")]
        WorkspaceRoute::Workflows => automation::workflows_page().into_any(),
        #[cfg(feature = "workflows")]
        WorkspaceRoute::Calendar => automation::calendar_page(Signal::stored(true)).into_any(),
        #[cfg(not(feature = "workflows"))]
        WorkspaceRoute::Routines | WorkspaceRoute::Workflows | WorkspaceRoute::Calendar => {
            ().into_any()
        }
        WorkspaceRoute::Project(id) => {
            crate::lazy_view::load("project", move || project(id.clone(), error_icon.clone()))
                .into_any()
        }
    }
}

#[cfg(not(any(feature = "conversation", feature = "hub")))]
#[leptos::wasm_split::wasm_split(project, wasm_split_path = leptos::wasm_split, fallible)]
fn project(id: String, error_icon: String) -> crate::lazy_view::ChunkView {
    Ok(projects::page(id, error_icon).into_any())
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
