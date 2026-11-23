use super::*;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Kind {
    File,
    Url,
    Computer,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub(super) struct ResultTarget {
    pub view: Kind,
    pub path: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

impl ResultTarget {
    pub fn file(path: String, name: String) -> Self {
        Self {
            view: Kind::File,
            path,
            name,
            agent: None,
        }
    }

    pub fn tab(&self) -> &'static str {
        match self.view {
            Kind::File => "files",
            Kind::Url => "preview",
            Kind::Computer => "computer",
        }
    }

    fn validate(mut self) -> Option<Self> {
        if self.name.encode_utf16().count() > 1024
            || self.path.encode_utf16().count() > 4096
            || self
                .path
                .bytes()
                .any(|byte| byte <= b' ' || byte == 127 || byte == b'\\')
        {
            return None;
        }
        if self.view == Kind::File {
            let parts: Vec<_> = self.path.split('/').collect();
            if !matches!(parts.as_slice(), ["", "api", "agents" | "channels" | "threads", scope, "panel", "files", file] if protocol::id(scope) && protocol::id(file))
            {
                return None;
            }
        } else {
            let url = web_sys::Url::new(&self.path).ok()?;
            if !matches!(url.protocol().as_str(), "http:" | "https:")
                || !url.username().is_empty()
                || !url.password().is_empty()
            {
                return None;
            }
            self.path = url.href();
        }
        if self.view != Kind::Computer {
            self.agent = None;
        }
        if self.agent.as_deref().is_some_and(|id| !protocol::id(id)) {
            return None;
        }
        Some(self)
    }
}

fn parse(params: &web_sys::UrlSearchParams) -> Result<Option<ResultTarget>, ()> {
    if !params.has("result") {
        return Ok(None);
    }
    if params.get_all("result").length() != 1 {
        return Err(());
    }
    serde_json::from_str::<ResultTarget>(&params.get("result").ok_or(())?)
        .map_err(|_| ())?
        .validate()
        .map(Some)
        .ok_or(())
}

pub(super) fn current() -> Result<Option<ResultTarget>, ()> {
    let search = window().location().search().map_err(|_| ())?;
    parse(&web_sys::UrlSearchParams::new_with_str(&search).map_err(|_| ())?)
}

fn navigate(chat: Chat, target: Option<ResultTarget>) {
    let Ok(location) = window()
        .location()
        .href()
        .and_then(|href| web_sys::Url::new(&href))
    else {
        return;
    };
    let params = location.search_params();
    if let Some(target) = &target {
        let Ok(raw) = serde_json::to_string(target) else {
            return;
        };
        params.set("result", &raw);
    } else if params.has("result") {
        params.delete("result");
    } else {
        return;
    }
    if window().location().href().ok().as_deref() != Some(&location.href()) {
        let _ = window().history().and_then(|history| {
            history.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&location.href()))
        });
    }
    sync(chat, target);
}

fn sync(chat: Chat, target: Option<ResultTarget>) {
    if target.is_some() {
        if !chat.details.get_untracked() {
            chat.details.set(true);
        }
        if chat.workspace.get_untracked() {
            chat.workspace.set(false);
        }
    }
    if chat.result.get_untracked() != target {
        chat.result.set(target);
    }
}

pub(super) fn open(chat: Chat, target: ResultTarget) {
    if let Some(target) = target.validate() {
        navigate(chat, Some(target));
    }
}

pub(super) fn clear(chat: Chat) {
    navigate(chat, None);
}

pub(super) fn connect(chat: Chat) {
    let pop = window_event_listener(leptos::ev::popstate, move |_| {
        if let Ok(target) = current() {
            sync(chat, target);
        }
    });
    on_cleanup(move || pop.remove());
    let mounted = js_sys::Date::now();
    let seen = StoredValue::new(0_u64);
    Effect::new(move |_| {
        let latest = chat.turns.with(|rows| {
            rows.iter()
                .flatten()
                .filter_map(|turn| turn.panel_file.as_ref())
                .filter(|file| {
                    file.seq > seen.get_value() && js_sys::Date::parse(&file.opened_at) >= mounted
                })
                .max_by_key(|file| file.seq)
                .cloned()
        });
        if let Some(file) = latest {
            seen.set_value(file.seq);
            if file.deleted == 0 && protocol::id(&file.id) {
                open(
                    chat,
                    ResultTarget::file(
                        format!("{}/panel/files/{}", chat.base(), file.id),
                        file.name,
                    ),
                );
            }
        }
    });
}

pub(super) fn follow(chat: Chat, event: web_sys::MouseEvent) {
    if event.default_prevented()
        || event.button() != 0
        || event.meta_key()
        || event.ctrl_key()
        || event.shift_key()
        || event.alt_key()
    {
        return;
    }
    let link = event
        .target()
        .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
        .and_then(|node| node.closest("a[data-minarbot-result]").ok().flatten())
        .and_then(|link| link.dyn_into::<web_sys::HtmlAnchorElement>().ok());
    let Some(url) = link.and_then(|link| web_sys::Url::new(&link.href()).ok()) else {
        return;
    };
    if window().location().origin().ok().as_deref() == Some(&url.origin())
        && window().location().pathname().ok().as_deref() == Some(&url.pathname())
    {
        if let Ok(Some(target)) = parse(&url.search_params()) {
            event.prevent_default();
            open(chat, target);
        }
    }
}
