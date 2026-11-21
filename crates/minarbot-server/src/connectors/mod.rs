pub mod calendar;
pub(crate) mod composio;
pub mod config;
mod journal;
pub mod mail;
pub mod mailbox;
pub mod runner;
mod slack;
mod telegram;

use crate::{
    data::{all, closed, error, exists, one, param, string, transaction, uuid},
    web::Failure,
    AppState,
};
use axum::{
    extract::{Request, State},
    http::Method,
    response::Response,
    routing::any,
    Router,
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
type Result<T> = std::result::Result<T, Failure>;

pub fn initialize(db: &Connection) -> Result<()> {
    composio::initialize(db)?;
    mailbox::initialize(db)
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/agents/{id}/routines/calendar.ics", any(entry))
        .route("/api/connected-apps", any(entry))
        .route("/api/projects/{project}/connected-apps", any(entry))
        .route(
            "/api/projects/{project}/connected-apps/accounts",
            any(entry),
        )
        .route(
            "/api/projects/{project}/connected-apps/connect/{toolkit}",
            any(entry),
        )
        .route(
            "/api/projects/{project}/connected-apps/disconnect/{account}",
            any(entry),
        )
        .route(
            "/api/projects/{project}/connected-apps/grant-all/{toolkit}",
            any(entry),
        )
        .route(
            "/api/projects/{project}/agents/{id}/connected-apps",
            any(entry),
        )
        .route(
            "/api/projects/{project}/agents/{id}/connected-apps/{toolkit}",
            any(entry),
        )
        .route(
            "/api/projects/{project}/agents/{id}/connected-app-policies",
            any(entry),
        )
        .route("/api/connected-apps/key", any(entry))
        .route("/api/connected-apps/key/remove", any(entry))
        .route("/api/connected-apps/accounts", any(entry))
        .route("/api/connected-apps/connect/{toolkit}", any(entry))
        .route("/api/connected-apps/disconnect/{account}", any(entry))
        .route("/api/connected-apps/grant-all/{toolkit}", any(entry))
        .route("/api/agents/{id}/connected-apps", any(entry))
        .route("/api/agents/{id}/connected-apps/{toolkit}", any(entry))
        .route("/api/agents/{id}/connected-app-policies", any(entry))
        .route("/api/agents/{id}/email", any(entry))
        .route("/api/agents/{id}/email/messages/{message}", any(entry))
        .route("/api/email/settings", any(entry))
        .route("/api/email/inbound", any(entry))
        .route(
            "/api/agents/{id}/turns/{turn}/app-approvals/{call}",
            any(entry),
        )
        .route(
            "/api/threads/{id}/turns/{turn}/app-approvals/{call}",
            any(entry),
        )
        .route(
            "/api/channels/{id}/turns/{turn}/app-approvals/{call}",
            any(entry),
        )
}
pub(crate) async fn entry(State(state): State<AppState>, request: Request) -> Response {
    let path_name = request.uri().path().to_owned();
    let agent_email = path_name == "/api/email/settings"
        || path_name.starts_with("/api/agents/") && path_name.split('/').nth(4) == Some("email");
    if state.auth.is_some() && request.uri().path().ends_with("/routines/calendar.ics") {
        return crate::canonical(&state, request).await;
    }
    let result = async {
        if state.auth.is_some()
            || !crate::allowed(
                &state,
                Some(&format!(
                    "http://{}",
                    crate::text_header(request.headers(), "host").unwrap_or("")
                )),
            ) && !state.config.allowed_origins.iter().any(|origin| {
                url::Url::parse(origin).ok().is_some_and(|u| {
                    u.authority() == crate::text_header(request.headers(), "host").unwrap_or("")
                })
            })
            || crate::text_header(request.headers(), "sec-fetch-site") == Some("cross-site")
        {
            return Err(error(
                403,
                "Access is only allowed through the local Minarbot address.",
            ));
        }
        if crate::turns::pending(&state) {
            return Err(error(503, "Update is being verified. Retry after restart."));
        }
        if request.headers().contains_key("authorization")
            && !request.uri().path().starts_with("/api/projects/")
        {
            return Err(error(403, "Request must originate from Minarbot."));
        }
        if request.uri().path() == mailbox::INBOUND {
            return mailbox::inbound(&state, request).await;
        }
        if agent_email
            && request.headers().contains_key("origin")
            && !crate::data::origin(&state, request.headers())
        {
            return Err(error(
                403,
                "Email access requires the local owner in Minarbot.",
            ));
        }
        let mut path = request.uri().path().to_owned();
        let project = path
            .strip_prefix("/api/projects/")
            .and_then(|s| s.split_once('/'))
            .map(|(id, rest)| (id.to_owned(), format!("/api/{rest}")));
        if let Some((id, canonical)) = &project {
            if request.headers().contains_key("authorization")
                || request.headers().contains_key("origin")
                    && !crate::data::origin(&state, request.headers())
            {
                return Err(error(
                    403,
                    "Project integrations require the local Owner in Minarbot.",
                ));
            }
            let guard = state.database.lock().map_err(|_| Failure::storage())?;
            let current = crate::data::projects::read(&guard.connection, id)?;
            if current["archived"] == true {
                return Err(error(409, "Project is archived."));
            }
            if let Some(agent) = canonical
                .strip_prefix("/api/agents/")
                .and_then(|s| s.split('/').next())
            {
                if !exists(
                    &guard.connection,
                    "SELECT 1 FROM local_project_members WHERE project=? AND agent=?",
                    [id, agent],
                )? {
                    return Err(error(403, "Agent is not a project member."));
                }
            }
            path = canonical.clone();
        }
        let method = request.method().clone();
        let mut query = crate::data::query(&request);
        if let Some((id, _)) = &project {
            query.retain(|(name, _)| name != "project");
            query.push(("project".into(), id.clone()));
        }
        let value = if method == Method::POST {
            if !crate::data::origin(&state, request.headers())
                || crate::text_header(request.headers(), "content-type")
                    .unwrap_or("")
                    .split(';')
                    .next()
                    != Some("application/json")
            {
                return Err(error(403, "Request must originate from Minarbot."));
            }
            let bytes = axum::body::to_bytes(request.into_body(), 64000)
                .await
                .map_err(|_| error(413, "Request is too large."))?;
            let value: Value =
                serde_json::from_slice(&bytes).map_err(|_| error(400, "Invalid JSON."))?;
            if !value.is_object() {
                return Err(error(400, "JSON object is required."));
            }
            if !value["id"].as_str().is_some_and(uuid) {
                return Err(error(400, "Invalid request ID."));
            }
            value
        } else if method == Method::GET {
            json!({})
        } else {
            return Err(error(405, "Method not allowed."));
        };
        if path.ends_with("/routines/calendar.ics") && method == Method::GET {
            let db = state.database.lock().map_err(|_| Failure::storage())?;
            return calendar::export(
                &db.connection,
                path.split('/').nth(3).unwrap_or(""),
                state.config.now(),
            );
        }
        if agent_email {
            return mailbox::owner_route(&state, &path, &method, &query, &value);
        }
        let result = composio::route(&state, &path, &method, &query, &value).await?;
        Ok(crate::json_response(200, result))
    }
    .await;
    let mut response = result.unwrap_or_else(|e: Failure| crate::local_error(e.status, &e.message));
    crate::security_headers(&mut response);
    response
}
mod media;
mod workflow;

#[cfg(test)]
mod test_support;
