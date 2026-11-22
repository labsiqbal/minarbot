#[cfg(feature = "workflows")]
use crate::automation;
use crate::{onboarding, projects, workspace_route::WorkspaceRoute};
use leptos::prelude::*;

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
        WorkspaceRoute::Project(id) => projects::page(id, error_icon).into_any(),
    }
}

pub fn heading(title: String, description: String) -> impl IntoView {
    view! { <header class="k-page-head">{heading_text(title, description)}</header> }
}

pub fn heading_text(title: String, description: String) -> impl IntoView {
    let heading = NodeRef::<leptos::html::H1>::new();
    heading.on_load(|heading| {
        let _ = heading.focus();
    });
    view! {
        <div class="k-page-head__text">
            <h1 node_ref=heading tabindex="-1">{title}</h1>
            {(!description.is_empty()).then(|| view! { <p>{description}</p> })}
        </div>
    }
}
