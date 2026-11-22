use url::form_urlencoded;

#[path = "../../web/src/routes.rs"]
mod routes;
pub use routes::SettingsRoute;

pub const NAMESPACE: &str = "/_leptos/";
pub const DOCUMENT: &str = "_leptos/appearance.html";

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
