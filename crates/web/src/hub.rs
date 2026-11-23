mod project;

use crate::connection_ui::{encode, field, form, Header, Icon, Icons, Row, Section};
use gloo_net::http::Request;
use leptos::{prelude::*, task::spawn_local};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use web_sys::Storage;

const RETURN: &str = "minarbot:hub-return";
const INVITE: &str = "minarbot:hub-invite";
const JOIN: &str = "minarbot:hub-join";
const BUTTON: &str = "k-btn k-btn--md k-btn--secondary";
const PRIMARY: &str = "k-btn k-btn--md k-btn--primary";

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Config {
    accounts_host: String,
    domain: String,
    development_identity: bool,
    colors: Vec<crate::appearance::ColorTheme>,
    icons: Icons,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Session {
    account_id: String,
    name: String,
    csrf: String,
    project_id: Option<String>,
    role: Option<String>,
}
impl Session {
    fn manager(&self) -> bool {
        matches!(self.role.as_deref(), Some("owner" | "admin"))
    }
    async fn post<T: DeserializeOwned>(&self, path: &str, value: Value) -> Result<T, Failure> {
        post(path, &self.csrf, value, None).await
    }
}

#[derive(Clone)]
struct Failure {
    status: u16,
    code: String,
    message: String,
}
impl Failure {
    fn message(message: &str) -> Self {
        Self {
            status: 0,
            code: String::new(),
            message: message.into(),
        }
    }
    fn access_ended(&self) -> bool {
        matches!(self.status, 401 | 403 | 404)
    }
}

async fn read<T: DeserializeOwned>(path: &str) -> Result<T, Failure> {
    decode(Request::get(path).send().await).await
}
async fn post<T: DeserializeOwned>(
    path: &str,
    csrf: &str,
    value: Value,
    token: Option<&str>,
) -> Result<T, Failure> {
    let mut request = Request::post(path).header("X-CSRF-Token", csrf);
    if let Some(token) = token {
        request = request.header("Authorization", &format!("Bearer {token}"));
    }
    let request = request
        .json(&value)
        .map_err(|_| Failure::message("Request could not be prepared."))?;
    decode(request.send().await).await
}
async fn decode<T: DeserializeOwned>(
    response: Result<gloo_net::http::Response, gloo_net::Error>,
) -> Result<T, Failure> {
    let response =
        response.map_err(|_| Failure::message("Minarbot connection lost. Try again."))?;
    if !response.ok() {
        let status = response.status();
        let value: Value = response.json().await.unwrap_or_default();
        return Err(Failure {
            status,
            code: value["error"]["code"].as_str().unwrap_or_default().into(),
            message: value["error"]["message"]
                .as_str()
                .unwrap_or("Request failed. Try again.")
                .into(),
        });
    }
    response
        .json()
        .await
        .map_err(|_| Failure::message("Hub returned invalid data. Try again."))
}
fn storage() -> Result<Storage, Failure> {
    window()
        .session_storage()
        .ok()
        .flatten()
        .ok_or_else(|| Failure::message("Browser storage is unavailable."))
}
fn recall(key: &str) -> Option<String> {
    storage().ok()?.get_item(key).ok().flatten()
}
fn remember(key: &str, value: &str) -> Result<(), Failure> {
    storage()?
        .set_item(key, value)
        .map_err(|_| Failure::message("Browser storage could not be updated."))
}
fn forget(key: &str) -> Result<(), Failure> {
    storage()?
        .remove_item(key)
        .map_err(|_| Failure::message("Browser storage could not be updated."))
}
fn hash_fragment() -> String {
    window().location().hash().unwrap_or_default()
}
fn remove_fragment(path: &str) {
    if let Ok(history) = window().history() {
        let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(path));
    }
}
fn project_url(config: StoredValue<Config>, host: &str, path: &str) -> String {
    config.with_value(|config| {
        let suffix = format!(".{}", config.domain);
        let valid = host == config.accounts_host
            || host.strip_suffix(&suffix).is_some_and(|slug| {
                (3..=63).contains(&slug.len())
                    && slug
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            });
        if valid {
            format!("https://{host}{path}")
        } else {
            "/".into()
        }
    })
}
fn alert(error: RwSignal<String>) -> impl IntoView {
    view! {
        <div class="k-alert k-alert--error" role="alert" hidden=move || error.with(String::is_empty)>
            <span class="k-alert__text"><span class="k-alert__body">{error}</span></span>
        </div>
    }
}
fn brand() -> impl IntoView {
    view! { <span class="hub-brand"><Icon name="brand" />"Minarbot"</span> }
}
fn badge(label: String) -> impl IntoView {
    view! { <span class="k-badge">{label}</span> }
}
fn dialog(ref_node: NodeRef<leptos::html::Dialog>) {
    if let Some(dialog) = ref_node.get_untracked() {
        let _ = dialog.show_modal();
    }
}
fn close_dialog(ref_node: NodeRef<leptos::html::Dialog>) {
    if let Some(dialog) = ref_node.get_untracked() {
        dialog.close();
    }
}
fn manager_roles(owner: bool) -> impl Iterator<Item = &'static str> {
    ["member", "viewer", "admin"]
        .into_iter()
        .take(if owner { 3 } else { 2 })
}

pub(crate) fn page(config: Config) -> impl IntoView {
    crate::appearance::initialize(StoredValue::new(config.colors.clone()));
    provide_context(config.icons.clone());
    let central = window().location().hostname().ok().as_deref() == Some(&config.accounts_host);
    let path = window().location().pathname().unwrap_or_default();
    let error = RwSignal::new(String::new());
    if central && matches!(path.as_str(), "/join" | "/nodes/link") {
        let _ = remember(RETURN, &path);
    }
    if path == "/invite" && !hash_fragment().is_empty() {
        let hash = hash_fragment();
        let params = web_sys::UrlSearchParams::new_with_str(hash.trim_start_matches('#')).ok();
        if let Some((invitation, token)) =
            params.and_then(|p| p.get("invitationId").zip(p.get("token")))
        {
            if invitation.len() == 36 && token.len() <= 128 {
                if let Err(failure) = remember(INVITE, &json!({"invitationId":invitation,"token":token,"expiresAt":js_sys::Date::now()+86_400_000.0}).to_string()) {
                    error.set(failure.message);
                }
            }
        }
        remove_fragment("/invite");
    }
    if path == "/join" && !hash_fragment().is_empty() {
        let hash = hash_fragment();
        if let Ok(decoded) = js_sys::decode_uri_component(hash.trim_start_matches('#')) {
            if let Some(value) = decoded.as_string().and_then(|raw| parse_join(&raw)) {
                let _ = remember(JOIN, &value.to_string());
            }
        }
        remove_fragment("/join");
    }
    let invitation = path == "/invite";
    let path = StoredValue::new(path);
    let config = StoredValue::new(config);
    let session = RwSignal::new(None::<Result<Session, Failure>>);
    spawn_local(async move {
        let endpoint = if invitation {
            "/api/invitation-session"
        } else {
            "/api/session"
        };
        session.set(Some(read(endpoint).await));
    });
    view! {
        {move || match session.get() {
            None => view! { <main class="hub-center" role="status">"Loading Minarbot…"</main> }.into_any(),
            Some(Ok(actor)) if central && path.with_value(|path| path == "/login") =>
                view! { <Login actor=Some(actor) config error /> }.into_any(),
            Some(Ok(actor)) if invitation => view! { <Invite actor error /> }.into_any(),
            Some(Ok(actor)) => if central {
                view! { <Accounts actor path=path.get_value() config error /> }.into_any()
            } else {
                project::page(actor, config, error).into_any()
            },
            Some(Err(failure)) if central => {
                if failure.status != 401 { error.set(failure.message); }
                view! { <Login actor=None config error /> }.into_any()
            },
            Some(Err(failure)) => {
                let denied = matches!(failure.status, 403 | 404);
                if failure.status != 401 { error.set(failure.message); }
                let target = config.with_value(|cfg| format!("https://{}/", cfg.accounts_host));
                view! { <main class="hub-center bs-root"><div class="k-page">
                    {brand()}<h1>{if denied { "Project unavailable" } else { "Your projects, your agents" }}</h1>
                    {alert(error)}
                    {if denied { view! { <a class=PRIMARY href=target>"Open accounts"</a> }.into_any() }
                    else { view! { <button class=PRIMARY type="button" on:click=move |_| {
                        if invitation { let _ = remember(RETURN, "/invite"); }
                        spawn_local(async move {
                            match read::<Value>("/auth/start").await {
                                Ok(value) => if let Some(login) = value["login"].as_str() { let _ = window().location().assign(login); },
                                Err(failure) => error.set(failure.message),
                            }
                        });
                    }>"Sign in"</button> }.into_any() }}
                </div></main> }.into_any()
            }
        }}
    }
}

async fn finish_login(actor: Session) -> Result<(), Failure> {
    let query = window().location().search().unwrap_or_default();
    let state = web_sys::UrlSearchParams::new_with_str(&query)
        .ok()
        .and_then(|query| query.get("state"));
    if let Some(state) = state {
        let result = actor
            .post::<Value>("/auth/handoff", json!({"state":state}))
            .await?;
        let callback = result["callback"]
            .as_str()
            .ok_or_else(|| Failure::message("Sign-in handoff failed."))?;
        let _ = window().location().assign(callback);
    } else {
        let target = recall(RETURN);
        forget(RETURN)?;
        let _ = window().location().assign(
            target
                .as_deref()
                .filter(|path| matches!(*path, "/join" | "/nodes/link"))
                .unwrap_or("/"),
        );
    }
    Ok(())
}

#[component]
fn Login(
    actor: Option<Session>,
    config: StoredValue<Config>,
    error: RwSignal<String>,
) -> impl IntoView {
    let busy = RwSignal::new(false);
    let submit = move |event: web_sys::SubmitEvent| {
        let Some(data) = form(&event) else {
            return;
        };
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        error.set(String::new());
        let token = field(&data, "token");
        spawn_local(async move {
            let result = async {
                let bootstrap: Value = read("/auth/bootstrap").await?;
                let csrf = bootstrap["csrf"]
                    .as_str()
                    .ok_or_else(|| Failure::message("Sign-in bootstrap failed."))?;
                post::<Value>("/auth/session", csrf, json!({}), Some(&token)).await?;
                finish_login(read("/api/session").await?).await
            }
            .await;
            if let Err(failure) = result {
                error.set(failure.message);
            }
            busy.set(false);
        });
    };
    let name = actor
        .as_ref()
        .map(|actor| format!("Signed in as {}.", actor.name));
    let title = if actor.is_some() {
        "Continue to Minarbot"
    } else {
        "Sign in to Minarbot"
    };
    let actions = if let Some(actor) = actor {
        view! {
            <p>{name}</p><button class=PRIMARY type="button" disabled=busy on:click=move |_| {
                let actor = actor.clone();
                busy.set(true);
                spawn_local(async move {
                    if let Err(failure) = finish_login(actor).await { error.set(failure.message); }
                    busy.set(false);
                });
            }>"Continue"</button>
        }
        .into_any()
    } else if config.with_value(|config| config.development_identity) {
        view! {
            <form class="hub-form" method="post" action="/auth/session" on:submit=submit>
                <label class="k-field"><span class="k-field__label">"Development identity token"</span>
                    <input class="k-input" name="token" type="password" autocomplete="one-time-code" required />
                </label>
                <button class=PRIMARY type="submit" disabled=busy>"Sign in"</button>
            </form>
        }.into_any()
    } else {
        view! { <p class="k-alert">"Pilot identity setup is pending. Contact the pilot operator."</p> }.into_any()
    };
    view! { <main class="hub-center bs-root"><div class="k-page">{brand()}<h1>{title}</h1>{alert(error)}{actions}</div></main> }
}

#[component]
fn Invite(actor: Session, error: RwSignal<String>) -> impl IntoView {
    let busy = RwSignal::new(false);
    let proof = recall(INVITE)
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .filter(|value| {
            value["expiresAt"]
                .as_f64()
                .is_some_and(|expires| expires > js_sys::Date::now())
                && value["invitationId"]
                    .as_str()
                    .is_some_and(|id| id.len() == 36)
                && value["token"]
                    .as_str()
                    .is_some_and(|token| !token.is_empty() && token.len() <= 128)
        });
    let name = format!("Accept as {}.", actor.name);
    view! { <main class="hub-center bs-root"><div class="k-page">
        {brand()}<h1>"Join this project"</h1><p>{name}</p>{alert(error)}
        {if let Some(proof) = proof {
            view! { <button class=PRIMARY type="button" disabled=busy on:click=move |_| {
                let actor = actor.clone();
                let body = json!({"invitationId":proof["invitationId"],"token":proof["token"]});
                busy.set(true);
                spawn_local(async move {
                    match actor.post::<Value>("/api/invitations/accept", body).await {
                        Ok(_) => { let _ = forget(INVITE); let _ = forget(RETURN); let _ = window().location().replace("/"); }
                        Err(failure) => { error.set(failure.message); busy.set(false); }
                    }
                });
            }>"Accept invitation"</button> }.into_any()
        } else { view! { <p role="alert">"Invitation unavailable. Ask for a new link."</p> }.into_any() }}
    </div></main> }
}

fn account_footer(actor: Session, error: RwSignal<String>) -> impl IntoView {
    let busy = RwSignal::new(false);
    let name = actor.name.clone();
    view! {
        <footer><span class="hub-account" title=name.clone()>{name.clone()}</span>
            <button class="k-icon-btn" type="button" aria-label="Sign out" disabled=busy on:click=move |_| {
                busy.set(true);
                let actor = actor.clone();
                spawn_local(async move {
                    match actor.post::<Value>("/auth/logout",json!({"global":false})).await {
                        Ok(_) => { let _ = window().location().reload(); }
                        Err(failure) => { error.set(failure.message); busy.set(false); }
                    }
                });
            }><Icon name="logout" /></button>
        </footer>
    }
}

#[component]
fn Accounts(
    actor: Session,
    path: String,
    config: StoredValue<Config>,
    error: RwSignal<String>,
) -> impl IntoView {
    let content = match path.as_str() {
        "/nodes/link" => view! { <NodeApproval actor=actor.clone() /> }.into_any(),
        "/join" => view! { <JoinAgent actor=actor.clone() config /> }.into_any(),
        _ => view! { <Projects actor=actor.clone() config /> }.into_any(),
    };
    view! { <div class="hub-shell bs-root">
        <aside class="hub-sidebar"><a class="hub-link" href="/">{brand()}</a>
            <nav aria-label="Account">
                <a class="hub-nav-item" href="/" aria-current=(path == "/").then_some("page")><Icon name="folder" />"Projects"</a>
                <a class="hub-nav-item" href="/nodes/link" aria-current=(path == "/nodes/link").then_some("page")><Icon name="monitor" />"Link machine"</a>
            </nav>
            {account_footer(actor, error)}
        </aside>
        <main class="hub-main">{alert(error)}{content}</main>
    </div> }
}

#[derive(Clone, Deserialize)]
struct Project {
    name: String,
    host: String,
    role: String,
}

#[component]
fn Projects(actor: Session, config: StoredValue<Config>) -> impl IntoView {
    let projects = RwSignal::new(None::<Vec<(Project, String)>>);
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let create = NodeRef::new();
    let slug = RwSignal::new(String::new());
    let domain = config.with_value(|config| config.domain.clone());
    // Resolve URL under the current owner before spawning the HTTP request.
    let domain_for_links = domain.clone();
    spawn_local(async move {
        match read::<Vec<Project>>("/api/projects").await {
            Ok(value) => projects.set(Some(
                value
                    .into_iter()
                    .filter_map(|project| {
                        let suffix = format!(".{domain_for_links}");
                        let address = project.host.strip_suffix(&suffix)?;
                        if !address
                            .bytes()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
                        {
                            return None;
                        }
                        let href = format!("https://{}/", project.host);
                        Some((project, href))
                    })
                    .collect(),
            )),
            Err(failure) => error.set(failure.message),
        }
    });
    let submit = move |event: web_sys::SubmitEvent| {
        let Some(data) = form(&event) else {
            return;
        };
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        error.set(String::new());
        let actor = actor.clone();
        let name = field(&data, "name");
        let address = field(&data, "slug");
        let href = project_url(config, &format!("{address}.{domain}"), "/");
        spawn_local(async move {
            match actor
                .post::<Value>("/api/projects", json!({"name":name,"slug":address}))
                .await
            {
                Ok(_) => {
                    let _ = window().location().assign(&href);
                }
                Err(failure) => {
                    error.set(failure.message);
                    busy.set(false);
                }
            }
        });
    };
    view! { <div class="k-page bs-root">
        <Header title=Signal::derive(|| String::from("Projects")) actions=Box::new(move || view! {
            <button class=PRIMARY type="button" on:click=move |_| dialog(create)><Icon name="plus" />"Create project"</button>
        }.into_any()) />
        {alert(error)}
        {move || match projects.get() {
            None if error.with(String::is_empty) => view! { <p role="status">"Loading projects…"</p> }.into_any(),
            None => ().into_any(),
            Some(rows) if rows.is_empty() => view! { <p>"No projects yet. Create a project or accept an invitation."</p> }.into_any(),
            Some(rows) => view! { <div class="k-card">{rows.into_iter().map(|(project, href)| {
                view! { <Row title=project.name icon="folder"
                    meta=Box::new(move || view! { <span>{project.host}</span>{badge(project.role)} }.into_any())
                    trailing=Box::new(move || view! { <a class="k-btn k-btn--secondary k-btn--sm hub-link" href=href>"Open project"<Icon name="external-link" /></a> }.into_any()) />
                }
            }).collect_view()}</div> }.into_any(),
        }}
        <dialog class="hub-form-dialog" node_ref=create aria-labelledby="hub-create-title"
            on:cancel=move |event: web_sys::Event| { if busy.get_untracked() { event.prevent_default(); } }>
          <div class="k-dialog k-dialog--md bs-root">
            <div class="k-dialog__head">
                <h2 class="k-dialog__title" id="hub-create-title">"Create project"</h2>
                <button type="button" class="k-icon-btn" aria-label="Close" disabled=busy on:click=move |_| close_dialog(create)><Icon name="close" /></button>
            </div>
            <div class="k-dialog__body"><form id="hub-create" class="hub-form" method="post" action="/api/projects" on:submit=submit>
                <label class="k-field"><span class="k-field__label">"Project name"</span><input class="k-input" name="name" required maxlength="120" autofocus /></label>
                <div class="k-field"><label class="k-field__label" for="hub-project-address">"Project address"</label>
                    <input class="k-input" id="hub-project-address" name="slug" required minlength="3" maxlength="63" pattern=r"[a-z][a-z0-9\-]{1,61}[a-z0-9]"
                        aria-describedby="hub-address-preview" on:input=move |event| slug.set(event_target_value(&event)) />
                    <span id="hub-address-preview" class="k-field__hint">{move || format!("{}.{}", slug.with(|s| if s.is_empty() { "your-project".into() } else { s.clone() }), config.with_value(|c| c.domain.clone()))}</span>
                </div>
                {alert(error)}
            </form></div>
            <div class="k-dialog__foot">
                <button class=BUTTON type="button" disabled=busy on:click=move |_| close_dialog(create)>"Cancel"</button>
                <button class=PRIMARY type="submit" form="hub-create" disabled=busy>"Create project"</button>
            </div>
          </div>
        </dialog>
    </div> }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    label: String,
    node_id: String,
    installation_id: String,
    fingerprint: String,
    capabilities: Vec<String>,
    expires_at: f64,
}

#[component]
fn NodeApproval(actor: Session) -> impl IntoView {
    let error = RwSignal::new(String::new());
    let code = RwSignal::new(String::new());
    let candidate = RwSignal::new(None::<Candidate>);
    let busy = RwSignal::new(false);
    let approved = RwSignal::new(false);
    let submit = move |event: web_sys::SubmitEvent| {
        if form(&event).is_none() || busy.get_untracked() {
            return;
        }
        busy.set(true);
        error.set(String::new());
        spawn_local(async move {
            let current = code.get_untracked();
            match read::<Candidate>(&format!(
                "/api/nodes/enrollment?humanCode={}",
                encode(&current)
            ))
            .await
            {
                Ok(result) if code.get_untracked() == current => candidate.set(Some(result)),
                Ok(_) => {}
                Err(failure) => error.set(failure.message),
            }
            busy.set(false);
        });
    };
    let actor = StoredValue::new(actor);
    view! { <div class="k-page bs-root">
        <Header title=Signal::derive(|| String::from("Link machine")) />{alert(error)}
        <Show when=move || approved.get() fallback=move || view! {
            <form class="hub-form" action="/api/nodes/enrollment" on:submit=submit>
                <label class="k-field"><span class="k-field__label">"Device code"</span>
                    <input class="k-input" name="humanCode" required pattern="[A-F0-9]{12}" maxlength="12" autocomplete="one-time-code"
                        prop:value=code on:input=move |event| { code.set(event_target_value(&event).trim().to_uppercase()); candidate.set(None); } />
                </label>
                <div><button class=BUTTON type="submit" disabled=busy>"Review machine"</button></div>
            </form>
            {move || candidate.get().map(|value| {
                let fingerprint = value.fingerprint.clone();
                let expires = js_sys::Date::new(&value.expires_at.into()).to_locale_time_string("en").as_string().unwrap_or_default();
                view! { <Section title=value.label>
                    <div class="k-card">
                        {[
                            ("Account",actor.with_value(|a| a.name.clone())),
                            ("Node",value.node_id), ("Installation", value.installation_id), ("Signing fingerprint",value.fingerprint),
                            ("Capabilities",value.capabilities.join(", ")), ("Expires",expires),
                        ].into_iter().map(|(title, value)| view! { <Row title=title trailing=Box::new(move || view! { <span class="hub-proof">{value}</span> }.into_any()) /> }).collect_view()}
                    </div>
                    <p class="k-alert">"Verify the signing fingerprint in your local Minarbot app before approving."</p>
                    <div><button class=PRIMARY type="button" disabled=busy on:click=move |_| {
                        let actor = actor.get_value();
                        let input = json!({"humanCode":code.get_untracked(),"fingerprint":fingerprint});
                        busy.set(true);
                        spawn_local(async move {
                            match actor.post::<Value>("/api/nodes/enrollment/approve",input).await {
                                Ok(_) => approved.set(true),
                                Err(failure) => error.set(failure.message),
                            }
                            busy.set(false);
                        });
                    }>"Approve machine"</button></div>
                </Section> }
            })}
        }><p role="status">"Machine approved. Return to your local Minarbot app to finish linking."</p></Show>
    </div> }
}

fn parse_join(raw: &str) -> Option<Value> {
    if raw.len() > 4096 {
        return None;
    }
    let value: Value = serde_json::from_str(raw).ok()?;
    let uuid = |key| {
        value[key]
            .as_str()
            .is_some_and(crate::workspace_route::valid_id)
    };
    let cost = |key| {
        value[key]
            .as_u64()
            .filter(|n| (1..=9_007_199_254_740_991).contains(n))
    };
    let name = value["name"].as_str()?;
    let digest = value["localPolicyDigest"].as_str()?;
    (uuid("nodeId")
        && uuid("agentId")
        && !name.trim().is_empty()
        && name.encode_utf16().count() <= 160
        && digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && cost("providerMaxCostMicros")? <= cost("maxCostMicros")?)
    .then_some(value)
}

#[derive(Clone, Deserialize, PartialEq)]
struct NamedSection {
    id: String,
    name: String,
}

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Member {
    account_id: String,
    name: String,
    role: String,
    revision: i64,
}

#[derive(Deserialize)]
struct ProjectOptions {
    sections: Vec<NamedSection>,
    members: Vec<Member>,
}

#[component]
fn JoinAgent(actor: Session, config: StoredValue<Config>) -> impl IntoView {
    let error = RwSignal::new(String::new());
    let Some(join) = recall(JOIN).and_then(|raw| parse_join(&raw)) else {
        return view! { <div class="k-page bs-root"><Header title=Signal::derive(|| String::from("Join an agent")) /><p>"Open Settings, Connections, minarbot.com in your local app, then choose an agent."</p></div> }.into_any();
    };
    let title = format!("Join with {}", join["name"].as_str().unwrap_or_default());
    let join = StoredValue::new(join);
    let projects = RwSignal::new(None::<Vec<Project>>);
    let host = RwSignal::new(String::new());
    let sections = RwSignal::new(Vec::<NamedSection>::new());
    let members = RwSignal::new(Vec::<Member>::new());
    let section = RwSignal::new(String::new());
    let selected = RwSignal::new(Vec::<String>::new());
    let invocation = RwSignal::new("owner".to_owned());
    let busy = RwSignal::new(false);
    let requested = RwSignal::new(false);
    let actor = StoredValue::new(actor);
    spawn_local(async move {
        match read::<Vec<Project>>("/api/projects").await {
            Ok(rows) => {
                let rows: Vec<_> = rows
                    .into_iter()
                    .filter(|row| row.role != "viewer")
                    .collect();
                if let Some(first) = rows.first() {
                    host.set(first.host.clone());
                }
                projects.set(Some(rows));
            }
            Err(failure) => error.set(failure.message),
        }
    });
    Effect::new(move |_| {
        let project = host.get();
        sections.set(Vec::new());
        members.set(Vec::new());
        selected.set(Vec::new());
        section.set(String::new());
        if project.is_empty() {
            return;
        }
        let actor = actor.get_value();
        spawn_local(async move {
            let result = actor
                .post::<ProjectOptions>("/api/project-options", json!({"projectHost":project}))
                .await;
            if host.try_get_untracked().as_deref() != Some(project.as_str()) {
                return;
            }
            match result {
                Ok(options) => {
                    if let Some(first) = options.sections.first() {
                        section.set(first.id.clone());
                    }
                    sections.set(options.sections);
                    members.set(
                        options
                            .members
                            .into_iter()
                            .filter(|m| m.account_id != actor.account_id && m.role != "viewer")
                            .collect(),
                    );
                }
                Err(failure) => error.set(failure.message),
            }
        });
    });
    let project_link = RwSignal::new(String::new());
    let submit = move |event: web_sys::SubmitEvent| {
        if form(&event).is_none()
            || busy.get_untracked()
            || section.with_untracked(String::is_empty)
        {
            return;
        }
        let project_host = host.get_untracked();
        if invocation.get_untracked() == "members" && selected.with_untracked(Vec::is_empty) {
            return;
        }
        busy.set(true);
        error.set(String::new());
        project_link.set(project_url(config, &project_host, "/#agents"));
        let actor = actor.get_value();
        let join = join.get_value();
        let mode = invocation.get_untracked();
        let invocation = if mode == "members" {
            json!({"mode":mode,"accountIds":selected.get_untracked()})
        } else {
            json!({"mode":mode})
        };
        let policy = json!({"sectionIds":[section.get_untracked()],"invocation":invocation,"scopes":["read","invoke"],"localPolicyDigest":join["localPolicyDigest"],"payerBindingRef":join["agentId"],"maxCostMicros":join["maxCostMicros"]});
        spawn_local(async move {
            let result = async {
                actor.post::<Value>("/api/agents", json!({"agentId":join["agentId"],"nodeId":join["nodeId"],"name":join["name"]})).await?;
                actor.post::<Value>("/api/participations", json!({"projectHost":project_host,"agentId":join["agentId"],"policy":policy,"providerMaxCostMicros":join["providerMaxCostMicros"]})).await
            }.await;
            match result {
                Ok(_) => {
                    let _ = forget(JOIN);
                    requested.set(true);
                }
                Err(failure) => error.set(failure.message),
            }
            busy.set(false);
        });
    };
    view! { <div class="k-page bs-root"><Header title=Signal::derive(move || title.clone()) />{alert(error)}
        <Show when=move || requested.get() fallback=move || view! {
            {move || match projects.get() {
                None => view! { <p role="status">"Loading projects…"</p> }.into_any(),
                Some(rows) if rows.is_empty() => view! { <p>"No projects yet. Accept a project invitation first."</p> }.into_any(),
                Some(rows) => view! { <form class="hub-form" method="post" action="/api/participations" on:submit=submit>
                    <label class="k-field"><span class="k-field__label">"Project"</span>
                        <select class="k-input k-select" name="project" prop:value=host on:change=move |event| host.set(event_target_value(&event))>
                            {rows.into_iter().map(|project| view! { <option value=project.host>{project.name}</option> }).collect_view()}
                        </select>
                    </label>
                    <label class="k-field"><span class="k-field__label">"Section"</span>
                        <select class="k-input k-select" name="section" prop:value=section on:change=move |event| section.set(event_target_value(&event))>
                            {move || sections.get().into_iter().map(|item| view! { <option value=item.id>{item.name}</option> }).collect_view()}
                        </select>
                    </label>
                    <label class="k-field"><span class="k-field__label">"Who can invoke this agent"</span>
                        <select class="k-input k-select" name="invocation" prop:value=invocation on:change=move |event| invocation.set(event_target_value(&event))>
                            <option value="owner">"Owner only"</option><option value="members">"Specific members"</option><option value="all-members">"All members"</option>
                        </select>
                    </label>
                    <Show when=move || invocation.get() == "members"><fieldset class="hub-form">
                        <legend class="k-field__label">"Allowed members"</legend>
                        {move || members.get().into_iter().map(|member| {
                            let id = member.account_id;
                            let checked = id.clone();
                            view! { <label><input type="checkbox" name="allowed" prop:checked=move || selected.with(|ids| ids.contains(&checked))
                                on:change=move |event| {
                                    let checked = event_target_checked(&event);
                                    selected.update(|ids| {
                                        ids.retain(|item| item != &id);
                                        if checked { ids.push(id.clone()); }
                                    });
                                } />{member.name}</label> }
                        }).collect_view()}
                    </fieldset></Show>
                    <p class="k-alert">"Dangerous tools require your approval in the local app."</p>
                    <div><button class=PRIMARY type="submit" disabled=move || busy.get() || section.with(String::is_empty) || invocation.get() == "members" && selected.with(Vec::is_empty)>"Request participation"</button></div>
                </form> }.into_any(),
            }}
        }><Section title="Request sent"><p role="status">"Review this request in your local Minarbot settings."</p><a class=BUTTON href=project_link>"Open project agents"</a></Section></Show>
    </div> }.into_any()
}
