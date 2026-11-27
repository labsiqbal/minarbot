mod calendar;
mod graph;
mod routines;
mod schedule;
mod workflows;

use crate::{
    api, connection_ui as ui, select::Select, system::kit, workspace, workspace_route::valid_id,
};
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};
use std::time::Duration;

pub(crate) fn routines_page() -> impl IntoView {
    crate::lazy_view::load("Routines", routines_screen)
}

pub(crate) fn workflows_page(active: Signal<bool>) -> impl IntoView {
    crate::lazy_view::load("Workflows", move || workflows_screen(active))
}

#[leptos::wasm_split::wasm_split(workflows_screen, wasm_split_path = leptos::wasm_split, fallible)]
fn workflows_screen(active: Signal<bool>) -> crate::lazy_view::ChunkView {
    Ok(workflows::workflows_page(active).into_any())
}

pub(crate) fn agent_library(
    owner: String,
    back: String,
    library: Option<js_sys::Promise>,
) -> impl IntoView {
    workflows::agent_library(owner, back, library)
}

#[leptos::wasm_split::wasm_split(routines, wasm_split_path = leptos::wasm_split, fallible)]
fn routines_screen() -> crate::lazy_view::ChunkView {
    Ok(routines::routines_page().into_any())
}

pub(crate) fn calendar_page(active: Signal<bool>) -> impl IntoView {
    crate::lazy_view::load("Calendar", move || calendar_screen(active))
}

#[leptos::wasm_split::wasm_split(calendar, wasm_split_path = leptos::wasm_split, fallible)]
fn calendar_screen(active: Signal<bool>) -> crate::lazy_view::ChunkView {
    Ok(calendar::calendar_page(active).into_any())
}

pub(crate) fn project_calendar(project: Value) -> impl IntoView {
    crate::lazy_view::load("Calendar", move || project_calendar_screen(project.clone()))
}

#[leptos::wasm_split::wasm_split(project_calendar, wasm_split_path = leptos::wasm_split, fallible)]
fn project_calendar_screen(project: Value) -> crate::lazy_view::ChunkView {
    Ok(view! { <calendar::Calendar project=Some(project) /> }.into_any())
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

fn rows(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

fn number(value: &Value) -> i64 {
    value.as_i64().unwrap_or_default()
}

fn enabled(value: &Value) -> bool {
    value == true || value == 1
}

fn options(values: &[(&str, &str)]) -> Vec<(String, String)> {
    values
        .iter()
        .map(|(id, name)| (id.to_string(), name.to_string()))
        .collect()
}

fn directory() -> Vec<(String, String)> {
    ui::local_agents()
        .into_iter()
        .filter(|agent| !agent.hidden)
        .map(|agent| (agent.id, agent.name))
        .collect()
}

fn url(path: &str, pairs: &[(&str, &str)]) -> String {
    let query = pairs
        .iter()
        .map(|(key, value)| format!("{key}={}", ui::encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{path}?{query}")
}

fn conversation(agent: &str, thread: &str) -> String {
    format!("/agents/{agent}/conversations/{thread}")
}

fn route_page(children: impl IntoView) -> impl IntoView {
    view! { <main class="chat page-view bs-root"><div class="page-scroll"><div class="page legacy is-wide">{children}</div></div></main> }
}

#[component]
fn PageHead(
    #[prop(into)] title: String,
    #[prop(optional, into)] description: String,
    #[prop(optional)] actions: Option<Children>,
) -> impl IntoView {
    view! {
        <header class="k-page-head">{workspace::heading_text(title, description)}
            {actions.map(|actions| view! { <div class="k-page-head__actions">{actions()}</div> })}
        </header>
    }
}

#[component]
fn ReadError(
    query: ui::Query<Value>,
    #[prop(default = "Retry")] label: &'static str,
) -> impl IntoView {
    view! {
        <Show when=move || !query.error.with(String::is_empty)>
            <kit::Alert tone="error" actions=kit::content(move || view! {
                <kit::Button label=kit::text(label) size="sm" on_click=move || query.reload.run(()) />
            })>{query.error}</kit::Alert>
        </Show>
    }
}

fn storage() -> Result<web_sys::Storage, String> {
    window()
        .session_storage()
        .ok()
        .flatten()
        .ok_or_else(|| "Browser recovery storage is unavailable.".into())
}

#[derive(Clone)]
struct Request {
    path: String,
    body: Value,
    record: Value,
}

impl Request {
    fn new(path: String, mut body: Value, record: impl FnOnce(&Value) -> Value) -> Self {
        body["id"] = json!(ui::uuid());
        Self {
            path,
            record: record(&body),
            body,
        }
    }

    fn restore(path: String, body: Value, record: Value) -> Result<Self, String> {
        if !body["id"].as_str().is_some_and(valid_id) {
            return Err("Saved request could not be read.".into());
        }
        Ok(Self { path, body, record })
    }
}

type Decode = fn(&str, Value) -> Result<Request, String>;

#[derive(Clone, Copy)]
struct Journal {
    key: StoredValue<String>,
    pending: RwSignal<Result<Option<Request>, String>>,
    error: RwSignal<String>,
    busy: RwSignal<bool>,
    conflict: RwSignal<Option<String>>,
}

impl Journal {
    fn new(key: String, decode: Decode) -> Self {
        let pending = storage().and_then(|storage| {
            let Some(raw) = storage
                .get_item(&key)
                .map_err(|_| "Saved request could not be read.")?
            else {
                return Ok(None);
            };
            let record =
                serde_json::from_str(&raw).map_err(|_| "Saved request could not be read.")?;
            decode(&key, record).map(Some)
        });
        Self {
            key: StoredValue::new(key),
            pending: RwSignal::new(pending),
            error: RwSignal::new(String::new()),
            busy: RwSignal::new(false),
            conflict: RwSignal::new(None),
        }
    }

    fn locked(self) -> bool {
        self.busy.get() || self.pending.with(|pending| !matches!(pending, Ok(None)))
    }

    fn submit(self, request: Request, done: Callback<Value>) {
        if !request.body["id"].as_str().is_some_and(valid_id) {
            self.error
                .set("Command could not be created. Reload Minarbot and try again.".into());
            return;
        }
        if self.busy.get_untracked()
            || self.pending.with_untracked(|pending| match pending {
                Ok(None) => false,
                Ok(Some(pending)) => pending.record != request.record,
                Err(_) => true,
            })
        {
            return;
        }
        let key = self.key.get_value();
        let raw = request.record.to_string();
        let saved = storage().and_then(|storage| {
            storage
                .set_item(&key, &raw)
                .map_err(|_| "Request could not be saved in browser storage.".to_owned())?;
            Ok(storage)
        });
        let storage = match saved {
            Ok(storage) => storage,
            Err(error) => {
                self.error.set(error);
                return;
            }
        };
        self.pending.set(Ok(Some(request.clone())));
        self.busy.set(true);
        self.error.set(String::new());
        self.conflict.set(None);
        spawn_local(async move {
            let result = api::post_status::<Value>(&request.path, &request.body).await;
            let settled = result
                .as_ref()
                .err()
                .is_none_or(|(status, _)| (400..500).contains(status));
            let cleared = !settled
                || match storage.get_item(&key) {
                    Ok(current) if current.as_deref() == Some(&raw) => {
                        storage.remove_item(&key).is_ok()
                    }
                    Ok(_) => true,
                    Err(_) => false,
                };
            if self.busy.try_get_untracked().is_none() {
                return;
            }
            self.busy.set(false);
            if settled && cleared {
                self.pending.set(Ok(None));
            }
            match result {
                Ok(value) if cleared => done.run(value),
                Ok(_) => self
                    .error
                    .set("Request was accepted, but recovery could not be cleared.".into()),
                Err((status, message)) => {
                    if status == 409 {
                        self.conflict.set(Some(request.path));
                    }
                    self.error.set(message);
                }
            }
        });
    }
}

#[component]
fn Recovery(
    journal: Journal,
    done: Callback<Value>,
    confirm: kit::Confirm,
    #[prop(optional)] replay: Option<Callback<Request>>,
) -> impl IntoView {
    view! {
        {move || match journal.pending.get() {
            Ok(Some(request)) => view! { <kit::Alert tone="warn" title=kit::text("Request is not confirmed yet") actions=kit::content(move || {
                let retry = request.clone();
                view! { <kit::Button label=kit::text("Retry request") busy=journal.busy on_click=move || {
                    if let Some(replay) = replay { replay.run(retry.clone()); }
                    else { journal.submit(retry.clone(), done); }
                } />
                    {discard(journal, confirm)} }
            })>"Retry the original request to check its result without duplicating work."</kit::Alert> }.into_any(),
            Err(error) => view! { <kit::Alert tone="error" title=kit::text("Recovery could not be read") actions=kit::content(move || discard(journal, confirm))>{error}</kit::Alert> }.into_any(),
            Ok(None) => ().into_any(),
        }}
        {kit::error(journal.error)}
    }
}

fn discard(journal: Journal, confirm: kit::Confirm) -> impl IntoView {
    view! {
        <kit::Button label=kit::text("Discard recovery") disabled=journal.busy on_click=move || kit::ask(Some(confirm),
            "Discard recovery?", "Check saved settings and run history first. The request may already have succeeded.",
            "Discard recovery", true, move || {
                match storage().and_then(|storage| storage.remove_item(&journal.key.get_value()).map_err(|_| "Recovery could not be cleared.".into())) {
                    Ok(()) => { journal.pending.set(Ok(None)); journal.error.set(String::new()); }
                    Err(error) => journal.error.set(error),
                }
            }) />
    }
}

fn date_at(at: i64) -> js_sys::Date {
    js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(at as f64))
}

fn iso(at: i64) -> String {
    date_at(at).to_iso_string().as_string().unwrap_or_default()
}

fn local_date(at: i64) -> String {
    let offset = date_at(at).get_timezone_offset() as i64 * 60_000;
    iso(at.saturating_sub(offset)).chars().take(19).collect()
}

fn status(value: &str) -> &str {
    match value {
        "queued" | "ready" => "Queued",
        "running" => "Running",
        "done" => "Done",
        "failed" => "Failed",
        "cancelled" => "Cancelled",
        "waiting" => "Waiting",
        "approval" => "Needs approval",
        "scheduled" => "Scheduled",
        "paused" => "Paused",
        "no_update" => "No update",
        other => other,
    }
}

thread_local! {
    static LIBRARY: std::cell::RefCell<Option<(String, js_sys::Promise, f64)>> =
        const { std::cell::RefCell::new(None) };
}

/// Fetches an agent's workflow library while its chat menu opens, so the
/// scoped Workflows page can start from it.
pub(crate) fn prefetch_library(owner: &str) {
    let fresh = LIBRARY.with_borrow(|slot| {
        slot.as_ref()
            .is_some_and(|(agent, _, at)| agent == owner && js_sys::Date::now() - at < 3000.0)
    });
    if !fresh {
        let request = window().fetch_with_str(&format!("/api/agents/{owner}/workflows"));
        LIBRARY.with_borrow_mut(|slot| {
            *slot = Some((owner.to_owned(), request, js_sys::Date::now()));
        });
    }
}

/// The prefetched library response for `owner`, if it is still fresh.
pub(crate) fn take_library(owner: &str) -> Option<js_sys::Promise> {
    LIBRARY
        .with_borrow_mut(Option::take)
        .filter(|(agent, _, at)| agent == owner && js_sys::Date::now() - at < 3000.0)
        .map(|(_, request, _)| request)
}

/// Routines, Workflows and the calendar. Each page stays mounted while the
/// others show, so switching keeps its state.
pub(crate) fn outlet() -> impl IntoView {
    let router = crate::router::get();
    let route = Memo::new(move |_| {
        router.map_or_else(
            || window().location().pathname().unwrap_or_default(),
            |router| router.with_url(|url| url.split('?').next().unwrap_or_default().to_owned()),
        )
    });
    let routines_active = Memo::new(move |_| route.with(|route| route == "/routines"));
    let routines_mounted = RwSignal::new(routines_active.get_untracked());
    Effect::watch(
        move || routines_active.get(),
        move |active, _, _| {
            if *active {
                routines_mounted.set(true);
                document().set_title("Routines · Minarbot");
            } else {
                // Hide routines at once and release its view after the destination paints.
                request_animation_frame(move || {
                    request_animation_frame(move || {
                        if routines_active.try_get_untracked() == Some(false) {
                            routines_mounted.try_set(false);
                        }
                    });
                });
            }
        },
        true,
    );
    let calendar_active = Memo::new(move |_| route.with(|route| route == "/calendar"));
    let calendar_visited = RwSignal::new(calendar_active.get_untracked());
    let workflows_active = Memo::new(move |_| route.with(|route| route == "/workflows"));
    let workflows_visited = RwSignal::new(workflows_active.get_untracked());
    Effect::new(move |_| {
        if calendar_active.get() {
            calendar_visited.set(true);
            document().set_title("Automation calendar · Minarbot");
        }
        if workflows_active.get() {
            workflows_visited.set(true);
            document().set_title("Workflows · Minarbot");
        }
    });
    view! {
        <Show when=move || routines_mounted.get()>
            <div class="automation-routines-route" hidden=move || !routines_active.get() inert=move || !routines_active.get()>
                {routines_page()}
            </div>
        </Show>
        <Show when=move || calendar_visited.get()>{calendar_page(calendar_active.into())}</Show>
        <Show when=move || workflows_visited.get()>
            <div class="automation-workflows-route" hidden=move || !workflows_active.get() inert=move || !workflows_active.get()>
                {workflows_page(workflows_active.into())}
            </div>
        </Show>
    }
}
