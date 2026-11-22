use crate::appearance::{self, ColorTheme};
use gloo_net::http::Request;
use leptos::prelude::*;
use serde::Deserialize;
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, HtmlInputElement, KeyboardEvent, UrlSearchParams};

const RETURN_FOCUS_KEY: &str = "minarbot:appearance-return-focus";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub colors: Vec<ColorTheme>,
    groups: Vec<Group>,
    search_icon: String,
    close_icon: String,
    error_icon: String,
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Directory {
    #[serde(default)]
    hub_available: bool,
}

fn buttons(container: &Element) -> Vec<HtmlElement> {
    let Ok(list) = container.query_selector_all("button:not([hidden])") else {
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

pub fn shell(config: Config, return_to: String) -> impl IntoView {
    let query = RwSignal::new(String::new());
    let hub = RwSignal::new(false);
    let input = NodeRef::<leptos::html::Input>::new();
    let nav = NodeRef::<leptos::html::Nav>::new();
    let return_to = StoredValue::new(return_to);
    let groups = StoredValue::new(config.groups);
    leptos::task::spawn_local(async move {
        if let Ok(response) = Request::get("/api/agents").send().await {
            if let Ok(directory) = response.json::<Directory>().await {
                hub.set(directory.hub_available);
            }
        }
    });

    let rail = groups.with_value(|rows| rows.iter().enumerate().map(|(index, group)| {
        let group_id = format!("settings-group-{index}");
        let heading_id = group_id.clone();
        let hidden = move || groups.with_value(|groups| {
            let group = &groups[index];
            !query.with(|query| group.items.iter().any(|item| visible(item, &group.label, query, hub.get())))
        });
        let items = group.items.iter().enumerate().map(|(item_index, item)| {
            let active = item.id == "appearance";
            let section = item.id.clone();
            view! {
                <button type="button" class="cfg-nav__item" class:is-active=active
                    aria-label=item.label.clone() title=item.label.clone() aria-current=active.then_some("page")
                    hidden=move || groups.with_value(|groups| {
                        let group = &groups[index];
                        !query.with(|query| visible(&group.items[item_index], &group.label, query, hub.get()))
                    })
                    on:click=move |_| {
                        if active {
                            if let Some(heading) = document().get_element_by_id("appearance-title").and_then(|element| element.dyn_into::<HtmlElement>().ok()) {
                                let _ = heading.focus();
                            }
                        } else if let Ok(params) = UrlSearchParams::new() {
                            return_to.with_value(|path| {
                                if path != "/" {
                                    params.set("from", path);
                                }
                            });
                            let suffix = params.to_string().as_string().filter(|value| !value.is_empty()).map(|value| format!("?{value}")).unwrap_or_default();
                            let _ = window().location().assign(&format!("/settings/{section}{suffix}"));
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
                    && !event.target().is_some_and(|target| target.is_instance_of::<HtmlInputElement>())
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
                        query.with(|query| groups.iter().any(|group| group.items.iter().any(|item| visible(item, &group.label, query, hub.get()))))
                    )>"No settings found"</p>
                </nav>
            </aside>
            <button type="button" class="k-icon-btn app-settings__close" aria-label="Close settings"
                on:click=move |_| return_to.with_value(|path| close(path))
                inner_html=config.close_icon>
            </button>
            <main class="app-settings__main">
                <div class="app-settings__page legacy app-settings__page--preferences">
                    {appearance::panel(config.colors, config.error_icon)}
                </div>
            </main>
        </dialog>
    }
}
