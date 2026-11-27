// One router for every workspace route in the app document.
use crate::{
    routes::SettingsRoute,
    workspace_route::{valid_id, WorkspaceRoute},
};
use leptos::prelude::*;
use std::cell::{Cell, RefCell};
use wasm_bindgen::{JsCast, JsValue};

/// What the outlet renders for a URL. Kinds that keep one view across
/// their own URLs (chat, automation) carry no payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RouteKind {
    /// `/`: first run, the agent list on a phone, or the default chat.
    Root,
    Setup,
    /// `/` on a phone: the agent list, with no chat beside it.
    AgentList,
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

/// The phone layout's media query. CSS uses the same width.
pub(crate) const PHONE: &str = "(max-width: 767px)";

fn phone_query() -> Option<web_sys::MediaQueryList> {
    window().match_media(PHONE).ok().flatten()
}

thread_local! {
    static ROUTER: Cell<Option<Router>> = const { Cell::new(None) };
    /// The URL of each history entry of this document, by depth, as far as
    /// this document has seen them.
    static ENTRIES: RefCell<Vec<Option<String>>> = const { RefCell::new(Vec::new()) };
}

fn depth() -> usize {
    window()
        .history()
        .and_then(|history| history.state())
        .ok()
        .and_then(|state| state.as_f64())
        .map_or(0, |depth| depth.max(0.0) as usize)
}

fn record(depth: usize, url: String, truncate: bool) {
    ENTRIES.with_borrow_mut(|entries| {
        if truncate {
            entries.truncate(depth + 1);
        }
        if entries.len() <= depth {
            entries.resize(depth + 1, None);
        }
        entries[depth] = Some(url);
    });
}

#[derive(Clone, Copy)]
pub(crate) struct Router {
    location: RwSignal<String>,
    background: RwSignal<Option<String>>,
    setup: RwSignal<bool>,
    phone: RwSignal<bool>,
}

impl Router {
    /// Installs the app router: one `popstate` listener and one interceptor
    /// for same-origin links to app routes. Lives as long as the document.
    pub(crate) fn install(setup: bool) -> Self {
        let query = phone_query();
        let router = Self {
            location: RwSignal::new(current()),
            background: RwSignal::new(None),
            setup: RwSignal::new(setup),
            phone: RwSignal::new(query.as_ref().is_some_and(web_sys::MediaQueryList::matches)),
        };
        ROUTER.with(|cell| cell.set(Some(router)));
        record(depth(), current(), false);
        let pop = window_event_listener(leptos::ev::popstate, move |_| {
            record(depth(), current(), false);
            router.set(current());
        });
        let click = window_event_listener(leptos::ev::click, move |event| router.intercept(event));
        // A host app (the phone app) opens a route and steps back through
        // these events; a handled event is cancelled.
        let open = window_event_listener(
            leptos::ev::Custom::<web_sys::CustomEvent>::new("minarbot:open"),
            move |event| {
                let path = event.detail().as_string();
                if let Some(path) = path.filter(|path| RouteKind::parse(path).is_some()) {
                    event.prevent_default();
                    router.navigate(&path);
                }
            },
        );
        let back = window_event_listener(
            leptos::ev::Custom::<web_sys::Event>::new("minarbot:back"),
            move |event| {
                if close_top_layer()
                    || router.can_go_back()
                        && window()
                            .history()
                            .and_then(|history| history.back())
                            .is_ok()
                {
                    event.prevent_default();
                }
            },
        );
        if let Some(query) = query {
            let change = wasm_bindgen::closure::Closure::<dyn Fn()>::new(move || {
                let matches = phone_query().is_some_and(|query| query.matches());
                if router.phone.get_untracked() != matches {
                    router.phone.set(matches);
                }
            });
            let _ =
                query.add_event_listener_with_callback("change", change.as_ref().unchecked_ref());
            change.forget();
        }
        // The app document never unmounts its router.
        std::mem::forget((pop, click, open, back));
        router
    }

    /// Whether the phone layout applies, tracked.
    pub(crate) fn phone(self) -> bool {
        self.phone.get()
    }

    /// Goes up to `parent`: back when the previous entry is that page,
    /// else in place of the current entry, so Back does not return here.
    pub(crate) fn up(self, parent: &str) {
        let depth = depth();
        let previous = ENTRIES.with_borrow(|entries| {
            depth
                .checked_sub(1)
                .and_then(|index| entries.get(index).cloned().flatten())
        });
        if previous.is_some_and(|url| path(&url) == path(parent))
            && self.can_go_back()
            && window()
                .history()
                .and_then(|history| history.back())
                .is_ok()
        {
            return;
        }
        self.replace(parent);
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
            RouteKind::Root if self.phone.get() => RouteKind::AgentList,
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
            let depth = if replace {
                depth as usize
            } else {
                depth as usize + 1
            };
            record(depth, current(), !replace);
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

/// Closes the topmost menu, sheet or dialog, as Escape or a phone's Back
/// would. Settings is a page, not an overlay, so it stays.
fn close_top_layer() -> bool {
    let last = |selector: &str| {
        document()
            .query_selector_all(selector)
            .ok()
            .and_then(|list| list.item(list.length().checked_sub(1)?))
            .and_then(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
    };
    if let Some(popover) = last(":popover-open") {
        return popover.hide_popover().is_ok();
    }
    if let Some(menu) = last("details[open]:has(> .k-pop)")
        .and_then(|menu| menu.dyn_into::<web_sys::HtmlDetailsElement>().ok())
    {
        menu.set_open(false);
        return true;
    }
    let Some(dialog) = last("dialog:modal:not(#appearance-dialog)")
        .and_then(|dialog| dialog.dyn_into::<web_sys::HtmlDialogElement>().ok())
    else {
        return false;
    };
    let Ok(cancel) = web_sys::Event::new_with_event_init_dict("cancel", &{
        let init = web_sys::EventInit::new();
        init.set_cancelable(true);
        init
    }) else {
        return false;
    };
    if dialog.dispatch_event(&cancel).unwrap_or(false) {
        dialog.close();
    }
    true
}
