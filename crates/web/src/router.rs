// One router for every workspace route in the app document.
use crate::{
    routes::SettingsRoute,
    workspace_route::{valid_id, WorkspaceRoute},
};
use leptos::prelude::*;
use std::cell::Cell;
use wasm_bindgen::{JsCast, JsValue};

/// What the outlet renders for a URL. Kinds that keep one view across
/// their own URLs (chat, automation) carry no payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RouteKind {
    /// `/`: first run or the default chat, decided by [`Router::setup`].
    Root,
    Setup,
    Conversation,
    Inbox(String),
    Remote(String, String),
    Automation,
    Project(String),
    Settings(SettingsRoute),
}

impl RouteKind {
    /// Parses a same-origin path with an optional query. `None` means the
    /// URL is not an app route and needs a document load.
    pub(crate) fn parse(url: &str) -> Option<Self> {
        let (path, query) = url.split_once('?').unwrap_or((url, ""));
        if let Some(section) = SettingsRoute::from_path(path) {
            return Some(Self::Settings(section));
        }
        if let Some(route) = WorkspaceRoute::from_path(path) {
            return match route {
                WorkspaceRoute::Setup => (!has_param(query, "computer")).then_some(Self::Root),
                WorkspaceRoute::Workflows if workflow_scope(url).is_some() => {
                    Some(Self::Conversation)
                }
                WorkspaceRoute::Routines | WorkspaceRoute::Workflows | WorkspaceRoute::Calendar => {
                    Some(Self::Automation)
                }
                WorkspaceRoute::Project(id) => Some(Self::Project(id)),
            };
        }
        let parts = path.strip_prefix('/')?.split('/').collect::<Vec<_>>();
        Some(match parts.as_slice() {
            ["agents", agent] if valid_id(agent) => Self::Conversation,
            ["agents", agent, "conversations", thread] if valid_id(agent) && valid_id(thread) => {
                Self::Conversation
            }
            ["agents", agent, "inbox"] if valid_id(agent) => {
                Self::Inbox(agent.to_ascii_lowercase())
            }
            ["rooms", room] if valid_id(room) => Self::Conversation,
            ["machines", machine, "agents", agent] if valid_id(machine) && valid_id(agent) => {
                Self::Remote(machine.to_ascii_lowercase(), agent.to_ascii_lowercase())
            }
            _ => return None,
        })
    }

    pub(crate) fn is_conversation(&self) -> bool {
        matches!(self, Self::Conversation | Self::Inbox(_) | Self::Remote(..))
    }
}

fn has_param(query: &str, name: &str) -> bool {
    web_sys::UrlSearchParams::new_with_str(query).is_ok_and(|params| params.has(name))
}

/// `/workflows?agent=<id>&from=<chat>`: the agent's workflow library over
/// its chat. Returns the owner and the chat to go back to.
pub(crate) fn workflow_scope(url: &str) -> Option<(String, String)> {
    let query = url.strip_prefix("/workflows?")?;
    let params = web_sys::UrlSearchParams::new_with_str(query).ok()?;
    let owner = params.get("agent")?.to_ascii_lowercase();
    if !valid_id(&owner) {
        return None;
    }
    let base = format!("/agents/{owner}");
    let back = params
        .get("from")
        .filter(|path| {
            path == &base
                || path
                    .strip_prefix(&format!("{base}/conversations/"))
                    .is_some_and(valid_id)
        })
        .unwrap_or(base);
    Some((owner, back))
}

/// The page under Settings: its `from` query when that is an app route,
/// else `/`.
pub(crate) fn settings_return(url: &str) -> String {
    let query = url.split_once('?').map_or("", |(_, query)| query);
    web_sys::UrlSearchParams::new_with_str(query)
        .ok()
        .and_then(|params| params.get("from"))
        .filter(|from| {
            !from.contains(['\\', '#'])
                && !from.chars().any(char::is_control)
                && RouteKind::parse(from)
                    .is_some_and(|kind| !matches!(kind, RouteKind::Settings(_)))
        })
        .unwrap_or_else(|| "/".into())
}

/// The browser's path and query.
pub(crate) fn current() -> String {
    let location = window().location();
    format!(
        "{}{}",
        location.pathname().unwrap_or_default(),
        location.search().unwrap_or_default()
    )
}

fn path(url: &str) -> &str {
    url.split('?').next().unwrap_or_default()
}

thread_local! {
    static ROUTER: Cell<Option<Router>> = const { Cell::new(None) };
}

#[derive(Clone, Copy)]
pub(crate) struct Router {
    location: RwSignal<String>,
    background: RwSignal<Option<String>>,
    setup: RwSignal<bool>,
}

impl Router {
    /// Installs the app router: one `popstate` listener and one interceptor
    /// for same-origin links to app routes. Lives as long as the document.
    pub(crate) fn install(setup: bool) -> Self {
        let router = Self {
            location: RwSignal::new(current()),
            background: RwSignal::new(None),
            setup: RwSignal::new(setup),
        };
        ROUTER.with(|cell| cell.set(Some(router)));
        let pop = window_event_listener(leptos::ev::popstate, move |_| router.set(current()));
        let click = window_event_listener(leptos::ev::click, move |event| router.intercept(event));
        // The app document never unmounts its router.
        std::mem::forget((pop, click));
        router
    }

    /// Path and query of the current entry, tracked.
    pub(crate) fn url(self) -> String {
        self.location.get()
    }

    pub(crate) fn with_url<R>(self, read: impl FnOnce(&String) -> R) -> R {
        self.location.with(read)
    }

    /// Whether Settings opened over a page of this document.
    pub(crate) fn has_background(self) -> bool {
        self.background.with_untracked(Option::is_some)
    }

    /// Whether `/` opens first run. Starts from the document and follows
    /// the agent directory.
    pub(crate) fn set_setup(self, setup: bool) {
        if self.setup.get_untracked() != setup {
            self.setup.set(setup);
        }
    }

    /// The route kind of a URL, with `/` resolved.
    pub(crate) fn kind_of(self, url: &str) -> Option<RouteKind> {
        RouteKind::parse(url).map(|kind| match kind {
            RouteKind::Root if self.setup.get() => RouteKind::Setup,
            RouteKind::Root => RouteKind::Conversation,
            kind => kind,
        })
    }

    pub(crate) fn kind(self) -> Option<RouteKind> {
        self.location.with(|url| self.kind_of(url))
    }

    /// The page the workspace shows: the current route, or under Settings
    /// the route it opened over.
    pub(crate) fn page(self) -> Option<String> {
        self.location.with(|url| {
            if matches!(RouteKind::parse(url), Some(RouteKind::Settings(_))) {
                self.background.get()
            } else {
                Some(url.clone())
            }
        })
    }

    /// The path the sidebar marks as current.
    pub(crate) fn page_path(self) -> String {
        self.location.with(|url| {
            if matches!(RouteKind::parse(url), Some(RouteKind::Settings(_))) {
                path(&settings_return(url)).to_owned()
            } else if let Some((_, back)) = workflow_scope(url) {
                back
            } else {
                path(url).to_owned()
            }
        })
    }

    pub(crate) fn navigate(self, url: &str) {
        self.go(url, false);
    }

    pub(crate) fn replace(self, url: &str) {
        self.go(url, true);
    }

    /// Whether the previous history entry belongs to this document.
    pub(crate) fn can_go_back(self) -> bool {
        window()
            .history()
            .and_then(|history| history.state())
            .ok()
            .and_then(|state| state.as_f64())
            .is_some_and(|depth| depth >= 1.0)
    }

    fn go(self, url: &str, replace: bool) {
        if url == current() {
            self.set(current());
            return;
        }
        let Ok(history) = window().history() else {
            let _ = window().location().assign(url);
            return;
        };
        let depth = history
            .state()
            .ok()
            .and_then(|state| state.as_f64())
            .unwrap_or(0.0);
        let changed = if replace {
            history.replace_state_with_url(&JsValue::from_f64(depth), "", Some(url))
        } else {
            history.push_state_with_url(&JsValue::from_f64(depth + 1.0), "", Some(url))
        };
        if changed.is_ok() {
            self.set(current());
        } else {
            let _ = window().location().assign(url);
        }
    }

    fn set(self, next: String) {
        let previous = self.location.get_untracked();
        if previous == next {
            return;
        }
        let settings = |url: &str| matches!(RouteKind::parse(url), Some(RouteKind::Settings(_)));
        if !settings(&next) {
            self.background.set(None);
        } else if !settings(&previous) {
            self.background.set(Some(previous));
        }
        self.location.set(next);
    }

    fn intercept(self, event: web_sys::MouseEvent) {
        if event.default_prevented()
            || event.button() != 0
            || event.ctrl_key()
            || event.meta_key()
            || event.shift_key()
            || event.alt_key()
        {
            return;
        }
        let Some(anchor) = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .and_then(|element| element.closest("a[href]").ok().flatten())
            .and_then(|element| element.dyn_into::<web_sys::HtmlAnchorElement>().ok())
        else {
            return;
        };
        if anchor.has_attribute("download")
            || !anchor.target().is_empty()
            || !anchor.hash().is_empty()
            || window().location().origin().ok().as_deref() != Some(anchor.origin().as_str())
            || anchor.has_attribute("data-minarbot-result")
                && window().location().pathname().ok().as_deref()
                    == Some(anchor.pathname().as_str())
        {
            return;
        }
        let url = format!("{}{}", anchor.pathname(), anchor.search());
        if RouteKind::parse(&url).is_none() {
            return;
        }
        event.prevent_default();
        if anchor.has_attribute("data-scoped-workflow-back")
            && self.can_go_back()
            && window()
                .history()
                .and_then(|history| history.back())
                .is_ok()
        {
            return;
        }
        self.navigate(&url);
    }
}

/// The app router, when this document has one.
pub(crate) fn get() -> Option<Router> {
    ROUTER.with(Cell::get)
}

/// Opens a same-origin path: in the app when it is an app route, else with
/// a document load.
pub(crate) fn open(url: &str) {
    match get() {
        Some(router) if RouteKind::parse(url).is_some() => router.navigate(url),
        _ => {
            let _ = window().location().assign(url);
        }
    }
}

/// Pushes a URL that stays on the current view, such as a result panel or
/// a project tab.
pub(crate) fn push(url: &str) -> bool {
    match get() {
        Some(router) => {
            router.navigate(url);
            true
        }
        None => window()
            .history()
            .and_then(|history| history.push_state_with_url(&JsValue::NULL, "", Some(url)))
            .is_ok(),
    }
}
