use url::form_urlencoded;

#[path = "../../web/src/routes.rs"]
mod routes;
pub use routes::SettingsRoute;
#[path = "../../web/src/workspace_route.rs"]
mod workspace_route;
use workspace_route::valid_id;
pub use workspace_route::WorkspaceRoute;

pub const NAMESPACE: &str = "/_leptos/";
pub const DOCUMENT: &str = "_leptos/appearance.html";
pub const WORKFLOWS_DOCUMENT: &str = "_leptos/workflows.html";
pub const CONVERSATION_DOCUMENT: &str = "_leptos/conversation.html";

pub fn conversation_route(path: &str, query: &str) -> bool {
    let computer = form_urlencoded::parse(query.as_bytes()).any(|(key, _)| key == "computer");
    if computer {
        return path == "/";
    }
    crate::navigation(path)
        && (path == "/"
            || path.starts_with("/rooms/")
            || path.starts_with("/agents/") && !path.ends_with("/inbox"))
}

pub fn conversation_document(html: &str, path: &str, query: &str) -> String {
    let mut params = form_urlencoded::Serializer::new(String::new());
    params.append_pair("ui", "react");
    params.extend_pairs(form_urlencoded::parse(query.as_bytes()).filter(|(key, _)| key != "ui"));
    html.replace(
        "__REACT_CONVERSATION__",
        &format!("{path}?{}", params.finish()).replace('&', "&amp;"),
    )
}

/// Pairing may load only the hashed startup module from the packaged document.
pub fn startup_asset(document: &str) -> Option<&str> {
    let (_, rest) = document.split_once(r#"<script type="module" src=""#)?;
    let (src, _) = rest.split_once('"')?;
    (src.ends_with(".js") && asset(src)).then_some(src)
}

pub const POLICY: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-src 'self' https: http:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

pub fn asset(path: &str) -> bool {
    let Some((name, hash, extension)) = path
        .strip_prefix("/_leptos/assets/")
        .and_then(|file| file.rsplit_once('.'))
        .and_then(|(stem, extension)| {
            let (name, hash) = stem.rsplit_once('.')?;
            Some((name, hash, extension))
        })
    else {
        return false;
    };
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        && hash.len() == 16
        && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        && matches!(extension, "css" | "js" | "wasm" | "woff2" | "svg")
}

pub fn react_override(query: &str) -> bool {
    form_urlencoded::parse(query.as_bytes()).any(|(key, value)| key == "ui" && value == "react")
}

pub fn route_title(state: &crate::AppState, path: &str, query: &str) -> Option<&'static str> {
    if let Some(section) = SettingsRoute::from_path(path) {
        return Some(section.title());
    }
    if form_urlencoded::parse(query.as_bytes())
        .any(|(key, _)| matches!(key.as_ref(), "result" | "computer"))
    {
        return None;
    }
    if path.starts_with("/projects/")
        && form_urlencoded::parse(query.as_bytes()).any(|(key, value)| {
            key == "projectTab" && matches!(value.as_ref(), "apps" | "environment" | "mcp")
        })
    {
        return None;
    }
    match WorkspaceRoute::from_path(path)? {
        WorkspaceRoute::Setup => {
            let db = state.database.lock().ok()?;
            let directory = crate::data::agents::list(state, &db.connection).ok()?;
            let ready = directory["agents"]
                .as_array()?
                .iter()
                .any(|agent| agent["runnable"] == true && agent["hidden"] != true);
            (!ready).then(|| WorkspaceRoute::Setup.title())
        }
        route => Some(route.title()),
    }
}

pub fn document_for_route(path: &str, query: &str) -> &'static str {
    if WorkspaceRoute::from_path(path).is_some_and(|route| {
        route.is_automation()
            || matches!(route, WorkspaceRoute::Project(_))
                && form_urlencoded::parse(query.as_bytes())
                    .any(|(key, value)| key == "projectTab" && value == "calendar")
    }) {
        WORKFLOWS_DOCUMENT
    } else {
        DOCUMENT
    }
}

pub fn document(html: &str, route: &str, title: &str, query: &str) -> String {
    let from = form_urlencoded::parse(query.as_bytes())
        .find_map(|(key, value)| (key == "from").then_some(value))
        .filter(|value| {
            let path = value.split('?').next().unwrap_or("");
            !value.contains(['\\', '#'])
                && !value.chars().any(char::is_control)
                && !path.starts_with("/settings/")
                && crate::navigation(path)
        });
    let return_to = from.as_deref().unwrap_or("/");
    let mut params = form_urlencoded::Serializer::new(String::new());
    params.append_pair("ui", "react");
    if let Some(from) = &from {
        params.append_pair("from", from);
    }
    for (key, value) in form_urlencoded::parse(query.as_bytes()) {
        if (route.starts_with("/projects/") && key == "projectTab" && value == "calendar")
            || (route == "/routines" && key == "agent" && valid_id(&value))
        {
            params.append_pair(&key, &value);
            break;
        }
    }
    let react = format!("{route}?{}", params.finish());
    let escape = |value: &str| {
        value
            .replace('&', "&amp;")
            .replace('"', "&quot;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let react = escape(&react);
    html.replace("__SETTINGS_TITLE__", title)
        .split("__RETURN_TO__")
        .map(|part| part.replace("__REACT_APPEARANCE__", &react))
        .collect::<Vec<_>>()
        .join(&escape(return_to))
}
