use crate::appearance::ColorTheme;
#[cfg(feature = "workspace")]
use crate::{appearance, general, routes::SettingsRoute};
use leptos::prelude::*;
use serde::Deserialize;
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, KeyboardEvent, UrlSearchParams};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub colors: Vec<ColorTheme>,
    groups: Vec<Group>,
    search_icon: String,
    close_icon: String,
    pub(crate) error_icon: String,
    #[serde(default)]
    pub(crate) connection_icons: crate::connection_ui::Icons,
    #[serde(default)]
    pub(crate) system_icons: crate::system::kit::Icons,
    #[serde(default)]
    mcp_scopes: Vec<crate::system::external::Scope>,
}

#[derive(Deserialize)]
struct Group {
    label: String,
    items: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    id: String,
    label: String,
    svg: String,
}

fn buttons(container: &Element) -> Vec<HtmlElement> {
    let Ok(list) = container.query_selector_all("button:not([hidden]):not(:disabled)") else {
        return Vec::new();
    };
    (0..list.length())
        .filter_map(|index| list.item(index)?.dyn_into().ok())
        .collect()
}

pub fn move_focus(event: &KeyboardEvent, container: &str, activate: bool) {
    let key = event.key();
    let direction = match key.as_str() {
        "ArrowUp" => -1,
        "ArrowDown" => 1,
        "ArrowLeft" if activate => -1,
        "ArrowRight" if activate => 1,
        "Home" | "End" => 0,
        _ => return,
    };
    let Some(target) = event
        .target()
        .and_then(|node| node.dyn_into::<HtmlElement>().ok())
    else {
        return;
    };
    let Some(container) = target.closest(container).ok().flatten() else {
        return;
    };
    let items = buttons(&container);
    let Some(index) = items.iter().position(|button| button == &target) else {
        return;
    };
    let next = match key.as_str() {
        "Home" => 0,
        "End" => items.len() - 1,
        _ => (index as isize + direction).rem_euclid(items.len() as isize) as usize,
    };
    event.prevent_default();
    let _ = items[next].focus();
    if activate {
        items[next].click();
    }
}

/// Closes Settings onto the page it opened over and returns focus to the
/// sidebar control that opens it.
#[cfg(feature = "workspace")]
fn close(return_to: &str) {
    crate::router::open(return_to);
    request_animation_frame(|| {
        let control = document()
            .query_selector(".sidebar-reveal:not([hidden])")
            .ok()
            .flatten()
            .or_else(|| {
                document()
                    .query_selector(".sidebar-settings")
                    .ok()
                    .flatten()
            });
        if let Some(control) = control.and_then(|control| control.dyn_into::<HtmlElement>().ok()) {
            let _ = control.focus();
        }
    });
}

fn visible(item: &Item, group: &str, query: &str, hub: bool) -> bool {
    let label = format!("{group} {}", item.label).to_lowercase();
    (item.id != "hub" || hub) && query.split_whitespace().all(|term| label.contains(term))
}

#[cfg(feature = "workspace")]
/// Settings over the workspace while the route is a Settings section.
pub fn overlay(config: Config, section: Memo<Option<SettingsRoute>>) -> impl IntoView {
    provide_context(config.connection_icons.clone());
    provide_context(config.system_icons);
    provide_context(StoredValue::new(config.mcp_scopes));
    let (confirm, confirmation) = crate::system::kit::confirmation();
    provide_context(confirm);
    // The section stays put while Settings closes.
    let route = RwSignal::new(section.get_untracked().unwrap_or(SettingsRoute::General));
    Effect::new(move |_| {
        if let Some(next) = section.get() {
            if route.get_untracked() != next {
                confirm.set(None);
                route.set(next);
            }
            document().set_title(&format!("{} · Minarbot", next.title()));
        }
    });
    // A page under Settings gets its title back when Settings closes.
    if crate::router::get().is_some_and(crate::router::Router::has_background) {
        let title = document().title();
        on_cleanup(move || document().set_title(&title));
    }
    let return_to = || crate::router::settings_return(&crate::router::current());
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    dialog.on_load(|dialog| {
        let connected = dialog.is_connected();
        let open = move || {
            if !dialog.open() && dialog.show_modal().is_ok() {
                focus_heading();
            }
        };
        if connected {
            open();
        } else {
            request_animation_frame(open);
        }
    });
    let query = RwSignal::new(String::new());
    let input = NodeRef::<leptos::html::Input>::new();
    let nav = NodeRef::<leptos::html::Nav>::new();
    let groups = StoredValue::new(config.groups);
    let colors = StoredValue::new(config.colors);
    let error_icon = StoredValue::new(config.error_icon);
    let main = NodeRef::<leptos::html::Main>::new();
    Effect::new(move |_| {
        let _ = route.get();
        if main.get().is_some() {
            focus_heading();
        }
    });
    let directory = use_context::<ReadSignal<Option<Result<crate::shell::Directory, String>>>>();
    let hub = move || {
        directory.is_some_and(|directory| {
            directory.with(|value| {
                value
                    .as_ref()
                    .and_then(|result| result.as_ref().ok())
                    .is_some_and(|directory| directory.hub_available)
            })
        })
    };

    let rail = groups.with_value(|rows| rows.iter().enumerate().map(|(index, group)| {
        let group_id = format!("settings-group-{index}");
        let heading_id = group_id.clone();
        let hidden = move || groups.with_value(|groups| {
            let group = &groups[index];
            !query.with(|query| group.items.iter().any(|item| visible(item, &group.label, query, hub())))
        });
        let items = group.items.iter().enumerate().map(|(item_index, item)| {
            let path = format!("/settings/{}", item.id);
            let target = SettingsRoute::from_path(&path);
            let active = move || target == Some(route.get());
            view! {
                <button type="button" class="cfg-nav__item" class:is-active=active
                    aria-label=item.label.clone() title=item.label.clone() aria-current=move || active().then_some("page")
                    hidden=move || groups.with_value(|groups| {
                        let group = &groups[index];
                        !query.with(|query| visible(&group.items[item_index], &group.label, query, hub()))
                    })
                    on:click=move |_| {
                        if target == Some(route.get_untracked()) {
                            focus_heading();
                        } else {
                            crate::router::open(&section_url(&path, &return_to()));
                        }
                    }>
                    <span class="appearance-icon" aria-hidden="true" inner_html=item.svg.clone()></span>
                    <span>{item.label.clone()}</span>
                </button>
            }
        }).collect_view();
        view! {
            <div class="app-settings__group" role="group" aria-labelledby=group_id hidden=hidden>
                <h2 id=heading_id>{group.label.clone()}</h2>
                {items}
            </div>
        }
    }).collect_view());

    view! {
        <dialog id="appearance-dialog" class="app-settings bs-root" node_ref=dialog
            aria-labelledby="settings-title"
            on:cancel=move |event: web_sys::Event| {
                event.prevent_default();
                close(&return_to());
            }
            on:keydown=move |event: KeyboardEvent| {
                if event.key() == "/" && !event.default_prevented() && !event.is_composing()
                    && !event.ctrl_key() && !event.meta_key() && !event.alt_key()
                    && event.target().and_then(|target| target.dyn_into::<Element>().ok()).is_none_or(|target|
                        target.closest("input, textarea, select, [contenteditable]:not([contenteditable=false])").ok().flatten().is_none()
                    )
                {
                    if let Some(input) = input.get().filter(|input| input.offset_width() > 0) {
                        event.prevent_default();
                        let _ = input.focus();
                    }
                }
            }>
            <aside class="app-settings__nav">
                <div id="settings-title" class="app-settings__heading">"Settings"</div>
                <div class="app-settings__search">
                    <span class="appearance-icon" aria-hidden="true" inner_html=config.search_icon></span>
                    <input type="search" aria-label="Search settings" aria-keyshortcuts="/" placeholder="Search"
                        node_ref=input
                        on:input=move |event| query.set(event_target_value(&event).to_lowercase())
                        on:keydown=move |event: KeyboardEvent| match event.key().as_str() {
                            "Escape" if !query.get_untracked().is_empty() => {
                                event.prevent_default();
                                if let Some(input) = input.get() { input.set_value(""); }
                                query.set(String::new());
                            }
                            "ArrowDown" | "Enter" => {
                                event.prevent_default();
                                if let Some(button) = nav.get().and_then(|nav| buttons(&nav).into_iter().next()) {
                                    let _ = button.focus();
                                }
                            }
                            _ => {}
                        }
                    />
                    <kbd aria-hidden="true">"/"</kbd>
                </div>
                <nav class="app-settings__groups" aria-label="Settings" node_ref=nav
                    on:keydown=move |event| move_focus(&event, "nav", false)>
                    {rail}
                    <p class="app-settings__no-results" role="status" hidden=move || groups.with_value(|groups|
                        query.with(|query| groups.iter().any(|group| group.items.iter().any(|item| visible(item, &group.label, query, hub()))))
                    )>"No settings found"</p>
                </nav>
            </aside>
            <button type="button" class="k-icon-btn app-settings__close" aria-label="Close settings"
                on:click=move |_| close(&return_to())
                inner_html=config.close_icon>
            </button>
            {move || {
                let section = route.get();
                let preferences = matches!(section, SettingsRoute::General | SettingsRoute::Appearance);
                let page = match section {
                    SettingsRoute::Appearance => appearance::panel(colors.get_value(), error_icon.get_value()).into_any(),
                    SettingsRoute::General => general::panel(error_icon.get_value()).into_any(),
                    SettingsRoute::Skills => crate::lazy_view::load("Skills", skills).into_any(),
                    SettingsRoute::Machines | SettingsRoute::Devices | SettingsRoute::Computer
                    | SettingsRoute::Updates | SettingsRoute::Usage | SettingsRoute::Data
                    | SettingsRoute::Backup | SettingsRoute::External =>
                        crate::lazy_view::load(section.title(), move || system(section)).into_any(),
                    _ => crate::lazy_view::load(section.title(), move || connections(section)).into_any(),
                };
                view! {
                    <main class="app-settings__main" node_ref=main>
                        <div class="app-settings__page legacy" class:app-settings__page--preferences=preferences>{page}</div>
                    </main>
                }
            }}
            {confirmation}
        </dialog>
    }
}

#[cfg(feature = "workspace")]
#[leptos::wasm_split::wasm_split(settings_skills, wasm_split_path = leptos::wasm_split, fallible)]
fn skills() -> crate::lazy_view::ChunkView {
    request_animation_frame(focus_heading);
    Ok(crate::skills::panel().into_any())
}

#[cfg(feature = "workspace")]
#[leptos::wasm_split::wasm_split(settings_system, wasm_split_path = leptos::wasm_split, fallible)]
fn system(section: SettingsRoute) -> crate::lazy_view::ChunkView {
    request_animation_frame(focus_heading);
    Ok(crate::system::panel(section))
}

#[cfg(feature = "workspace")]
#[leptos::wasm_split::wasm_split(settings_connections, wasm_split_path = leptos::wasm_split, fallible)]
fn connections(section: SettingsRoute) -> crate::lazy_view::ChunkView {
    request_animation_frame(focus_heading);
    Ok(match section {
        SettingsRoute::Providers => crate::provider_instances::panel().into_any(),
        SettingsRoute::Vault => crate::vault::panel().into_any(),
        SettingsRoute::Email => crate::email::panel().into_any(),
        SettingsRoute::Hub => crate::hub_connection::panel().into_any(),
        SettingsRoute::ConnectedApps => crate::connected_apps::panel().into_any(),
        SettingsRoute::Mcp => crate::mcp_connections::panel().into_any(),
        _ => ().into_any(),
    })
}

pub fn section_url(path: &str, from: &str) -> String {
    if from != "/" {
        if let Ok(params) = UrlSearchParams::new() {
            params.set("from", from);
            if let Some(query) = params.to_string().as_string() {
                return format!("{path}?{query}");
            }
        }
    }
    path.to_owned()
}

pub fn focus_heading() {
    if let Some(heading) = document()
        .query_selector("#appearance-dialog .app-settings__page h1")
        .ok()
        .flatten()
        .and_then(|element| element.dyn_into::<HtmlElement>().ok())
    {
        let _ = heading.focus();
    }
}
