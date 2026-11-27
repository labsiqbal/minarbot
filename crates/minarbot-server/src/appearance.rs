use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};
use url::form_urlencoded;

#[path = "../../web/src/routes.rs"]
mod routes;
pub use routes::SettingsRoute;
#[path = "../../web/src/workspace_route.rs"]
mod workspace_route;
pub use workspace_route::WorkspaceRoute;

pub const NAMESPACE: &str = "/_leptos/";
/// The one Leptos document for every workspace route, pairing included.
pub const APP_DOCUMENT: &str = "_leptos/app.html";
/// The document title when the route names none (chat, results, Computer).
pub const DEFAULT_TITLE: &str = "Conversation";

pub fn log_retired_selector() {
    if std::env::var_os("MINARBOT_APPEARANCE_CLIENT").is_some() {
        eprintln!("Ignoring deprecated MINARBOT_APPEARANCE_CLIENT. Minarbot uses Leptos.");
    }
}

pub fn recovery_page() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Html(include_str!("../../web/recovery.html")),
    )
        .into_response()
}

/// Pairing may load only the hashed startup module from the packaged document.
pub fn startup_asset(document: &str) -> Option<&str> {
    let (_, rest) = document.split_once(r#"<script type="module" src=""#)?;
    let (src, _) = rest.split_once('"')?;
    (src.ends_with(".js") && asset(src)).then_some(src)
}

pub async fn load_document(dist: &std::path::Path, name: &str) -> Option<String> {
    let html = tokio::fs::read_to_string(dist.join(name)).await.ok()?;
    let startup = startup_asset(&html)?;
    let file = dist.join(startup.trim_start_matches('/'));
    tokio::fs::metadata(file)
        .await
        .ok()?
        .is_file()
        .then_some(html)
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

pub fn route_title(state: &crate::AppState, path: &str, query: &str) -> Option<&'static str> {
    if let Some(section) = SettingsRoute::from_path(path) {
        return Some(section.title());
    }
    if form_urlencoded::parse(query.as_bytes())
        .any(|(key, _)| matches!(key.as_ref(), "result" | "computer"))
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

/// Fills the app document for one route. `title` comes from [`route_title`];
/// `Set up Minarbot` also tells the client to open first run at `/`.
pub fn document(html: &str, title: Option<&str>, query: &str) -> String {
    let initial = if title == Some(WorkspaceRoute::Setup.title()) {
        "setup"
    } else {
        "workspace"
    };
    let title = title.unwrap_or(DEFAULT_TITLE);
    let from = form_urlencoded::parse(query.as_bytes())
        .find_map(|(key, value)| (key == "from").then_some(value))
        .filter(|value| {
            let path = value.split('?').next().unwrap_or("");
            !value.contains(['\\', '#'])
                && !value.chars().any(char::is_control)
                && path != "/settings"
                && !path.starts_with("/settings/")
                && crate::navigation(path)
        });
    let return_to = from.as_deref().unwrap_or("/");
    let escape = |value: &str| {
        value
            .replace('&', "&amp;")
            .replace('"', "&quot;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    html.replace("__SETTINGS_TITLE__", title)
        .replace("__RETURN_TO__", &escape(return_to))
        .replace("__INITIAL__", initial)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HTML: &str = r#"<title>__SETTINGS_TITLE__ · Minarbot</title><div id="leptos-root" data-return-to="__RETURN_TO__" data-initial="__INITIAL__"></div>"#;

    #[test]
    fn untitled_routes_get_the_conversation_title() {
        let html = document(HTML, None, "");
        assert!(
            html.contains("<title>Conversation · Minarbot</title>"),
            "{html}"
        );
        assert!(html.contains(r#"data-initial="workspace""#), "{html}");
        assert!(!html.contains("__"), "{html}");
    }

    #[test]
    fn setup_title_opens_first_run() {
        let html = document(HTML, Some(WorkspaceRoute::Setup.title()), "");
        assert!(
            html.contains("<title>Set up Minarbot · Minarbot</title>"),
            "{html}"
        );
        assert!(html.contains(r#"data-initial="setup""#), "{html}");
    }

    #[test]
    fn the_settings_list_is_a_route_but_never_the_page_to_return_to() {
        let title = SettingsRoute::from_path("/settings").map(SettingsRoute::title);
        assert_eq!(title, Some("Settings"));
        for from in ["%2Fsettings", "%2Fsettings%2Fgeneral"] {
            let html = document(HTML, title, &format!("from={from}"));
            assert!(html.contains(r#"data-return-to="/""#), "{html}");
        }
    }

    #[test]
    fn settings_keep_their_title_and_escaped_return_path() {
        let html = document(
            HTML,
            SettingsRoute::from_path("/settings/providers").map(SettingsRoute::title),
            "from=%2Fagents%2F00000000-0000-4000-8000-000000000000%3Fx%3D%22",
        );
        assert!(
            html.contains("<title>Providers · Minarbot</title>"),
            "{html}"
        );
        assert!(html.contains(r#"data-initial="workspace""#), "{html}");
        assert!(
            html.contains("/agents/00000000-0000-4000-8000-000000000000?x=&quot;"),
            "{html}"
        );
    }
}
