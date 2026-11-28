//! The General time zone and the dropdown that picks a zone.

use leptos::prelude::*;
use serde::Deserialize;

pub(crate) const PATH: &str = "/api/settings/time-zone";

#[derive(Clone, Default, Deserialize, PartialEq)]
pub(crate) struct Settings {
    /// The chosen zone; `None` follows the machine.
    pub zone: Option<String>,
    pub machine: String,
    pub effective: String,
    pub zones: Vec<String>,
}

/// A zone dropdown whose first option, the empty value, names `first`.
/// A saved zone missing from `zones` stays selectable.
#[component]
pub(crate) fn ZoneSelect(
    id: &'static str,
    value: RwSignal<String>,
    #[prop(into)] first: Signal<String>,
    #[prop(into)] zones: Signal<Vec<String>>,
    #[prop(optional, into)] disabled: Signal<bool>,
    #[prop(optional_no_strip)] described_by: Option<&'static str>,
    change: Callback<String>,
) -> impl IntoView {
    view! {
        <span class="connection-select">
            <select id=id name=id class="k-input" disabled=disabled aria-describedby=described_by
                prop:value=move || value.get()
                on:change=move |event| { let next = event_target_value(&event); value.set(next.clone()); change.run(next); }>
                <option value="" prop:selected=move || value.with(String::is_empty)>{first}</option>
                {move || {
                    let current = value.get();
                    (!current.is_empty() && !zones.with(|zones| zones.contains(&current)))
                        .then(|| view! { <option value=current.clone() selected>{current.replace('_', " ")}</option> })
                }}
                {move || zones.get().into_iter().map(|zone| {
                    let selected = zone.clone();
                    let label = zone.replace('_', " ");
                    view! { <option value=zone prop:selected=move || value.with(|value| *value == selected)>{label}</option> }
                }).collect_view()}
            </select>
            <span class="k-select__chev" aria-hidden="true"><crate::connection_ui::Icon name="chevron-down" /></span>
        </span>
    }
}
