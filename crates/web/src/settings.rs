use crate::appearance::{self, ColorTheme};
use crate::{general, routes::SettingsRoute};
use leptos::prelude::*;
use serde::Deserialize;
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, KeyboardEvent, UrlSearchParams};

const RETURN_FOCUS_KEY: &str = "minarbot:appearance-return-focus";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub colors: Vec<ColorTheme>,
    pub workspace_icons: crate::shell::Icons,
    groups: Vec<Group>,
    search_icon: String,
    close_icon: String,
    pub(crate) error_icon: String,
    #[serde(default)]
    connection_icons: crate::connection_ui::Icons,
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

fn close(return_to: &str) {
    if let Some(storage) = window().session_storage().ok().flatten() {
        let _ = storage.set_item(RETURN_FOCUS_KEY, return_to);
    }
    let _ = window().location().assign(return_to);
}

fn visible(item: &Item, group: &str, query: &str, hub: bool) -> bool {
    let label = format!("{group} {}", item.label).to_lowercase();
    (item.id != "hub" || hub) && query.split_whitespace().all(|term| label.contains(term))
}

pub fn shell(config: Config, return_to: String, route: SettingsRoute) -> impl IntoView {
    provide_context(config.connection_icons.clone());
    let route = RwSignal::new(route);
    let listener = window_event_listener(leptos::ev::popstate, move |_| {
        if let Some(route_now) = current() {
            route.set(route_now);
            document().set_title(&format!("{} · Minarbot", route_now.title()));
        }
    });
    on_cleanup(move || listener.remove());
    let query = RwSignal::new(String::new());
    let input = NodeRef::<leptos::html::Input>::new();
    let nav = NodeRef::<leptos::html::Nav>::new();
    let return_to = StoredValue::new(return_to);
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
                            let url = return_to.with_value(|from| section_url(&path, from));
                            if let Some(next) = target {
                                if window().history().and_then(|history| history.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&url))).is_ok() {
                                    route.set(next);
                                    document().set_title(&format!("{} · Minarbot", next.title()));
                                    return;
                                }
                            }
                            let _ = window().location().assign(&url);
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
        <dialog id="appearance-dialog" class="app-settings bs-root"
            aria-labelledby="settings-title"
            on:cancel=move |event: web_sys::Event| {
                event.prevent_default();
                return_to.with_value(|path| close(path));
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
                on:click=move |_| return_to.with_value(|path| close(path))
                inner_html=config.close_icon>
            </button>
            {move || {
                let page = match route.get() {
                    SettingsRoute::Appearance => appearance::panel(colors.get_value(), error_icon.get_value()).into_any(),
                    SettingsRoute::General => general::panel(error_icon.get_value()).into_any(),
                    SettingsRoute::Providers => crate::providers::panel().into_any(),
                    SettingsRoute::Engines => crate::engines::panel().into_any(),
                    SettingsRoute::CraftAnything => crate::craft_anything::panel().into_any(),
                    SettingsRoute::Vault => crate::vault::panel().into_any(),
                    SettingsRoute::Email => crate::email::panel().into_any(),
                    SettingsRoute::Hub => crate::hub_connection::panel().into_any(),
                    SettingsRoute::ConnectedApps => crate::connected_apps::panel().into_any(),
                    SettingsRoute::Mcp => crate::mcp_connections::panel().into_any(),
                    SettingsRoute::Skills => crate::skills::panel().into_any(),
                };
                view! {
                    <main class="app-settings__main" node_ref=main>
                        <div class="app-settings__page legacy" class:app-settings__page--preferences=move || matches!(route.get(), SettingsRoute::Appearance | SettingsRoute::General)>{page}</div>
                    </main>
                }
            }}
        </dialog>
    }
}

pub fn linked_section_url(path: &str) -> String {
    let from = document()
        .get_element_by_id("leptos-root")
        .and_then(|root| root.get_attribute("data-return-to"));
    section_url(path, from.as_deref().unwrap_or("/"))
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

fn current() -> Option<SettingsRoute> {
    SettingsRoute::from_path(&window().location().pathname().ok()?)
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
