use crate::{api, connection_ui::*};
use leptos::{prelude::*, task::spawn_local};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const PATH: &str = "/api/hub-link";

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Status {
    enrolled: bool,
    connected: bool,
    disabled: bool,
    node_id: Option<String>,
    account_id: Option<String>,
    account_name: Option<String>,
    origin: Option<String>,
    fingerprint: Option<String>,
    human_code: Option<String>,
    verification_uri: Option<String>,
    interval_ms: Option<f64>,
    expires_at: Option<f64>,
    #[serde(default)]
    approvals: Vec<Approval>,
}
#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Approval {
    action_id: String,
    action_digest: String,
    kind: String,
    payload: Value,
    requester_account_id: String,
    expires_at: f64,
}
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Grant {
    project_id: String,
    participation_id: String,
    local_agent_id: String,
    provider_max_cost_micros: u64,
    policy: Policy,
    #[serde(flatten)]
    rest: serde_json::Map<String, Value>,
}
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Policy {
    max_cost_micros: u64,
    invocation: Invocation,
    #[serde(flatten)]
    rest: serde_json::Map<String, Value>,
}
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Invocation {
    mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account_ids: Option<Vec<String>>,
}
#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct PendingRequest {
    project_name: String,
    project_host: String,
    sections: Vec<Named>,
    requester_name: String,
    grant: Grant,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Named {
    name: String,
}

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct History {
    agent_name: String,
    project_id: String,
    section_id: String,
    requester_account_id: String,
    state: String,
    text: String,
}

fn who(grant: &Grant) -> &'static str {
    match grant.policy.invocation.mode.as_str() {
        "owner" => "Owner only",
        "members" => "Specific members",
        _ => "All members",
    }
}

#[derive(Clone, Copy)]
struct Hub {
    status: RwSignal<Option<Status>>,
    pending: RwSignal<Vec<PendingRequest>>,
    saved: RwSignal<Vec<Grant>>,
    origin: RwSignal<String>,
    error: RwSignal<String>,
    notice: RwSignal<&'static str>,
    busy: RwSignal<bool>,
    loading: RwSignal<bool>,
    unavailable: RwSignal<bool>,
    generation: RwSignal<u32>,
    confirmation: Confirm,
}

impl Hub {
    async fn refresh(self, generation: u32) {
        match api::read_status::<Status>(PATH).await {
            Ok(status) => {
                if self.generation.try_get_untracked() != Some(generation) {
                    return;
                }
                if let Some(origin) = &status.origin {
                    if self.status.with_untracked(|previous| {
                        previous.as_ref().and_then(|status| status.origin.as_ref()) != Some(origin)
                    }) {
                        self.origin.set(origin.clone());
                    }
                }
                self.unavailable.set(false);
                self.error.set(String::new());
                let enrolled = status.enrolled;
                self.status.maybe_update(|current| {
                    if current.as_ref() == Some(&status) {
                        false
                    } else {
                        *current = Some(status);
                        true
                    }
                });
                if enrolled {
                    let grants = api::read::<Vec<Grant>>("/api/hub-link/grants").await;
                    let pending = api::read::<Vec<PendingRequest>>("/api/hub-link/requests").await;
                    if self.generation.try_get_untracked() != Some(generation) {
                        return;
                    }
                    match (grants, pending) {
                        (Ok(grants), Ok(pending)) => {
                            let grants = grants
                                .into_iter()
                                .filter(|grant| {
                                    grant.rest.get("enabled").and_then(Value::as_bool) == Some(true)
                                })
                                .collect();
                            self.saved.maybe_update(|rows| {
                                if *rows == grants {
                                    false
                                } else {
                                    *rows = grants;
                                    true
                                }
                            });
                            self.pending.maybe_update(|rows| {
                                if *rows == pending {
                                    false
                                } else {
                                    *rows = pending;
                                    true
                                }
                            });
                        }
                        (Err(message), _) | (_, Err(message)) => self.error.set(message),
                    }
                }
            }
            Err((403, _)) if self.generation.try_get_untracked() == Some(generation) => {
                self.status.set(None);
                self.unavailable.set(true);
            }
            Err((_, message)) if self.generation.try_get_untracked() == Some(generation) => {
                self.error.set(message)
            }
            _ => {}
        }
    }

    fn reload(self) {
        if self.busy.get_untracked() || self.loading.get_untracked() {
            return;
        }
        let generation = self.generation.get_untracked();
        self.loading.set(true);
        spawn_local(async move {
            self.refresh(generation).await;
            self.loading.try_set(false);
        });
    }

    fn run(self, action: &'static str, body: Value, notice: &'static str) {
        if self.busy.get_untracked() {
            return;
        }
        self.busy.set(true);
        self.error.set(String::new());
        self.generation.update(|generation| *generation += 1);
        let generation = self.generation.get_untracked();
        spawn_local(async move {
            let response = api::post::<Value>(&format!("{PATH}/{action}"), &body).await;
            if self.generation.try_get_untracked() != Some(generation) {
                return;
            }
            match response {
                Ok(result) => {
                    self.notice.set(notice);
                    if let Ok(status) = serde_json::from_value::<Status>(result) {
                        if let Some(origin) = &status.origin {
                            self.origin.set(origin.clone());
                        }
                        self.status.set(Some(status));
                    } else {
                        self.refresh(generation).await;
                    }
                }
                Err(message) => self.error.set(message),
            }
            self.busy.try_set(false);
        });
    }
}

impl Grant {
    fn revoke(&self) -> Value {
        json!({ "projectId": self.project_id, "participationId": self.participation_id })
    }
}

fn row_text(title: &'static str, details: String) -> impl IntoView {
    view! { <Row title=title trailing=Box::new(move || details.into_any()) /> }
}

fn link(label: &'static str, url: String) -> impl IntoView {
    view! {
        <a class="k-btn k-btn--secondary k-btn--md hub-link" href=url target="_blank" rel="noreferrer">
            {label}<Icon name="external-link" />
        </a>
    }
}

pub fn panel() -> impl IntoView {
    let hub = Hub {
        status: RwSignal::new(None),
        pending: RwSignal::new(Vec::new()),
        saved: RwSignal::new(Vec::new()),
        origin: RwSignal::new("https://accounts.minarbot.com".into()),
        error: RwSignal::new(String::new()),
        notice: RwSignal::new(""),
        busy: RwSignal::new(false),
        loading: RwSignal::new(false),
        unavailable: RwSignal::new(false),
        generation: RwSignal::new(0),
        confirmation: Confirm::new(None),
    };
    hub.reload();
    poll(3, move || {
        if !hub.unavailable.get_untracked() {
            hub.reload();
        }
    });
    let next_poll = RwSignal::new(0.0);
    let reload = UiAction::new(move |()| hub.reload());
    let linked = move || {
        hub.status.with(|status| {
            status
                .as_ref()
                .is_some_and(|status| status.enrolled && !status.disabled)
        })
    };
    view! {
        <div class="k-page bs-root hub-local">
            <Header title="minarbot.com".to_owned() />
            <ErrorNotice error=hub.error.into() retry=reload />
            <span class="sr-only" role="status">{hub.notice}</span>
            <Section title="This machine">
                {move || {
                    let status = hub.status.get();
                    let connected = status.as_ref().is_some_and(|status| status.connected);
                    let label = match status.as_ref() {
                        _ if hub.unavailable.get() => "Not available on this machine",
                        None => "Checking…",
                        Some(status) if status.disabled => "Unlinked",
                        Some(status) if status.connected => "Connected",
                        Some(status) if status.enrolled => "Offline",
                        Some(status) if status.human_code.is_some() => "Awaiting approval",
                        Some(_) => "Not linked",
                    };
                    let account = status.as_ref().and_then(|status| status.account_id.clone().map(|id| (id, status.account_name.clone().unwrap_or_else(|| "Linked account".into()))));
                    let origin = status.as_ref().and_then(|status| status.origin.as_deref().map(host));
                    let trailing = status.as_ref().filter(|status| !status.disabled).map(|status| {
                        if status.enrolled {
                            let unlink = UiAction::new(move |()| confirm(hub.confirmation, "Unlink this machine?",
                                "Stops hub turns and revokes this node. Re-enrollment requires a separate recovery flow.",
                                "Unlink machine", true, UiAction::new(move |()| hub.run("revoke", json!({}), ""))));
                            view! { <Menu label="Manage hub connection" disabled=hub.busy items=vec![MenuItem::new("Unlink machine", unlink).danger()] /> }.into_any()
                        } else {
                            view! { <Button label="Link machine" variant="primary" busy=hub.busy
                                click=UiAction::new(move |()| hub.run("enrollment/start", json!({}), "")) /> }.into_any()
                        }
                    });
                    let code = status.filter(|status| !status.enrolled && status.human_code.is_some());
                    view! {
                        <div class="k-card">
                            <Row title="Hub account" icon="link" meta=Box::new(move || view! {
                                <Badge success=connected>{label}</Badge>
                                {account.map(|(id, name)| view! { <span title=id tabindex="0">{name}</span> })}
                                {origin.map(|origin| view! { <span>{origin}</span> })}
                            }.into_any()) trailing=Box::new(move || trailing.into_any()) />
                        </div>
                        {code.map(|code| {
                            let expiry = code.expires_at.unwrap_or_default();
                            let interval = code.interval_ms.unwrap_or(5000.0);
                            let url = code.verification_uri.as_deref().and_then(external_url);
                            let time = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(expiry)).to_locale_time_string("default").as_string().unwrap_or_default();
                            view! {
                                <div class="k-card">
                                    <Row title="Device code" trailing=Box::new(move || view! { <strong class="hub-proof">{code.human_code}</strong> }.into_any()) />
                                    {code.fingerprint.map(|fingerprint| view! { <Row title="Signing fingerprint"
                                        meta=Box::new(move || view! { <span class="hub-proof">{fingerprint}</span> }.into_any()) /> })}
                                    {row_text("Expires", time)}
                                </div>
                                <div class="hub-actions">
                                    {url.map(|url| link("Approve in browser", url))}
                                    <Button label="Check approval" busy=hub.busy disabled=Signal::derive(move || js_sys::Date::now() >= expiry) click=UiAction::new(move |()| {
                                        let now = js_sys::Date::now();
                                        if now >= expiry { return; }
                                        if now < next_poll.get_untracked() { hub.error.set("Wait before checking the device code again.".into()); return; }
                                        next_poll.set(now + interval);
                                        hub.run("enrollment/poll", json!({}), "");
                                    }) />
                                </div>
                            }
                        })}
                    }
                }}
            </Section>
            <Show when=linked>{move || join_project(hub)}</Show>
            {move || hub.status.with(|status| status.as_ref().map(|status| status.approvals.clone())).filter(|rows| !rows.is_empty()).map(|rows| view! {
                <Section title="Local approvals">
                    {rows.into_iter().map(|approval| {
                        let description = format!("Requested by {}", approval.requester_account_id);
                        let payload = serde_json::to_string_pretty(&approval.payload).unwrap_or_default();
                        let expires = approval.expires_at;
                        let approve = json!({ "actionId": approval.action_id, "actionDigest": approval.action_digest, "approved": true });
                        let mut deny = approve.clone();
                        deny["approved"] = json!(false);
                        view! { <div class="k-card">
                            <Row title=approval.kind meta=Box::new(move || view! {
                                <span>{description}</span><Badge>"Owner approval required"</Badge>
                            }.into_any()) />
                            <Code label="Requested action" text=payload />
                            <div class="hub-actions">
                                <Button label="Deny action" busy=hub.busy click=UiAction::new(move |()| hub.run("approvals", deny.clone(), "")) />
                                <Button label="Approve action" busy=hub.busy disabled={expires <= js_sys::Date::now()} click=UiAction::new(move |()| {
                                    let approve = approve.clone();
                                    confirm(hub.confirmation, "Approve this local action?",
                                        "Allow this exact action once on your machine. Review its payload before approving.",
                                        "Approve action", false, UiAction::new(move |()| {
                                            if expires > js_sys::Date::now() { hub.run("approvals", approve.clone(), ""); }
                                        }));
                                }) />
                            </div>
                        </div> }
                    }).collect_view()}
                </Section>
            })}
            <Show when=move || hub.status.with(|status| status.as_ref().is_some_and(|status| status.enrolled))>
                {move || history()}
            </Show>
            <ConfirmDialog state=hub.confirmation />
        </div>
    }
}

fn join_project(hub: Hub) -> impl IntoView {
    let agent = agent_selection(false);
    let directory = use_context::<ReadSignal<Option<Result<crate::shell::Directory, String>>>>();
    let ceiling = RwSignal::new("10000".to_owned());
    let cost = RwSignal::new("100".to_owned());
    let url = RwSignal::new(None::<String>);
    let choosing = RwSignal::new(false);
    let connected = move || {
        hub.status
            .with(|status| status.as_ref().is_some_and(|status| status.connected))
    };
    let choose = UiAction::new(move |()| {
        let selected = directory
            .and_then(|directory| directory.get_untracked())
            .and_then(Result::ok)
            .and_then(|directory| {
                directory
                    .agents
                    .into_iter()
                    .find(|row| row.id == agent.get_untracked())
            });
        let Some(selected) = selected else { return };
        if !connected() || choosing.get_untracked() || hub.busy.get_untracked() {
            return;
        }
        let origin = hub.origin.get_untracked();
        if web_sys::Url::new(&origin).ok().is_none_or(|url| {
            url.protocol() != "https:"
                || url.origin() != origin
                || !url.username().is_empty()
                || !url.password().is_empty()
        }) {
            hub.error.set("Use the exact HTTPS accounts URL.".into());
            return;
        }
        let positive = |value: RwSignal<String>| {
            value.with_untracked(|value| {
                value
                    .parse::<u64>()
                    .ok()
                    .filter(|value| (1..=(1u64 << 53) - 1).contains(value))
            })
        };
        let (Some(ceiling), Some(cost)) = (positive(ceiling), positive(cost)) else {
            hub.error.set(
                "Set positive spending limits with provider cost within the turn ceiling.".into(),
            );
            return;
        };
        if cost > ceiling {
            hub.error.set(
                "Set positive spending limits with provider cost within the turn ceiling.".into(),
            );
            return;
        }
        let node_id = hub
            .status
            .with_untracked(|status| status.as_ref().and_then(|status| status.node_id.clone()));
        choosing.set(true);
        spawn_local(async move {
            let result =
                api::read::<Value>(&format!("{PATH}/agents/{}/policy", encode(&selected.id))).await;
            if choosing.try_set(false).is_some() {
                return;
            }
            if agent.get_untracked() != selected.id || hub.origin.get_untracked() != origin {
                return;
            }
            match result {
                Ok(policy) => {
                    let join = json!({ "nodeId": node_id, "agentId": selected.id, "name": selected.name,
                        "localPolicyDigest": policy["localPolicyDigest"], "maxCostMicros": ceiling, "providerMaxCostMicros": cost });
                    url.set(Some(format!("{origin}/join#{}", encode(&join.to_string()))));
                }
                Err(message) => hub.error.set(message),
            }
        });
    });
    view! {
        <Section title="Join a project">
            <Field label="Agent" name="hub-agent">
                <Select name="hub-agent" value=agent.into() options=Signal::derive(move || agent_options(false))
                    change=Callback::new(move |value| { agent.set(value); url.set(None); }) />
            </Field>
            <Disclosure title="Hub and spending limits">
                <div class="hub-form">
                    <Field label="Accounts URL" name="hub-origin">
                        <input class="k-input" id="hub-origin" type="url" prop:value=hub.origin
                            on:input=move |event| { hub.origin.set(event_target_value(&event)); url.set(None); } />
                    </Field>
                    <Field label="Turn ceiling (micros)" name="hub-ceiling">
                        <input class="k-input" id="hub-ceiling" type="number" min="1" step="1" prop:value=ceiling
                            on:input=move |event| ceiling.set(event_target_value(&event)) />
                    </Field>
                    <Field label="Provider request upper bound (micros)" name="hub-provider-cost">
                        <input class="k-input" id="hub-provider-cost" type="number" min="1" step="1" prop:value=cost
                            on:input=move |event| cost.set(event_target_value(&event)) />
                    </Field>
                </div>
            </Disclosure>
            <div class="hub-actions">
                <Button label="Choose project and policy" disabled=move || agent.with(String::is_empty) || hub.busy.get() || !connected() busy=choosing click=choose />
                {move || url.get().map(|url| link("Continue in hub", url))}
            </div>
            <Section title="Pending requests">
                <Show when=move || hub.pending.with(Vec::is_empty)><Empty title="No pending requests" /></Show>
                <For each=move || hub.pending.get() key=|item| (item.grant.participation_id.clone(), item.grant.rest.get("revision").cloned().map(|v| v.to_string()))
                    children=move |request| pending_request(hub, request) />
            </Section>
            <Show when=move || !hub.saved.with(Vec::is_empty)>
                <div class="k-card">
                    {move || hub.saved.get().into_iter().map(|grant| {
                        let name = local_agents().into_iter().find(|row| row.id == grant.local_agent_id).map_or_else(|| "Agent".into(), |row| row.name);
                        let label = who(&grant);
                        let revoke = grant.revoke();
                        let action = UiAction::new(move |()| {
                            let body = revoke.clone();
                            confirm(hub.confirmation, "Withdraw participation?", "Stops hub work for this agent in this project.",
                                "Withdraw", true, UiAction::new(move |()| hub.run("grants/revoke", body.clone(), "")));
                        });
                        view! { <Row title=name meta=Box::new(move || view! { <Badge>{label}</Badge><span>{grant.project_id}</span> }.into_any())
                            trailing=Box::new(move || view! { <Menu label="Manage participation" disabled=hub.busy
                                items=vec![MenuItem::new("Withdraw participation", action).danger()] /> }.into_any()) /> }
                    }).collect_view()}
                </div>
            </Show>
        </Section>
    }
}

fn pending_request(hub: Hub, request: PendingRequest) -> impl IntoView {
    let grant = request.grant;
    let invocation = who(&grant);
    let ceiling = grant.policy.max_cost_micros;
    let cost = grant.provider_max_cost_micros;
    let agent = StoredValue::new(grant.local_agent_id.clone());
    let agent_name = move || {
        local_agents()
            .into_iter()
            .find(|row| agent.with_value(|id| id == &row.id))
            .map(|row| row.name)
    };
    let approved = serde_json::to_value(&grant).unwrap_or_default();
    let declined = grant.revoke();
    let description = format!("{invocation} may invoke this agent in {} with a {ceiling}-micro turn ceiling and {cost}-micro provider request bound. Dangerous tools require local approval.", request.project_name);
    let sections = request
        .sections
        .into_iter()
        .map(|section| section.name)
        .collect::<Vec<_>>()
        .join(", ");
    let members = grant
        .policy
        .invocation
        .account_ids
        .filter(|_| grant.policy.invocation.mode == "members")
        .map(|rows| rows.join(", "));
    let approve = UiAction::new(move |()| {
        let body = approved.clone();
        confirm(
            hub.confirmation,
            "Approve project participation?",
            description.clone(),
            "Approve participation",
            false,
            UiAction::new(move |()| {
                hub.run(
                    "grants/approve",
                    body.clone(),
                    "Local participation approved. Project approval is also required.",
                )
            }),
        );
    });
    view! { <div class="k-card">
        <Row title=request.project_name meta=Box::new(move || view! {
            <span>{sections}</span><span>{format!("Requested by {}", request.requester_name)}</span><Badge>{invocation}</Badge>
        }.into_any()) trailing=Box::new(move || view! { <div class="hub-actions">
            <Button label="Decline" disabled=hub.busy click=UiAction::new(move |()| hub.run("grants/revoke", declined.clone(), "Participation declined.")) />
            <Button label="Approve" variant="primary" busy=hub.busy disabled=move || agent_name().is_none() ||
                !hub.status.with(|status| status.as_ref().is_some_and(|status| status.connected)) click=approve />
        </div> }.into_any()) />
        <Disclosure title="Request details">
            {move || row_text("Agent", agent_name().unwrap_or_else(|| "Unavailable agent".into()))}
            <Row title="Project address" meta=Box::new(move || request.project_host.into_any()) />
            {members.map(|members| view! { <Row title="Allowed members" meta=Box::new(move || members.into_any()) /> })}
            {row_text("Turn ceiling", format!("{ceiling} micros"))}
            {row_text("Provider request upper bound", format!("{cost} micros"))}
            <Row title="Tool approval" icon="shield" trailing=Box::new(|| "Local owner required".into_any()) />
        </Disclosure>
    </div> }
}

fn history() -> impl IntoView {
    let history = query::<Vec<History>>(|| "/api/hub-link/history".into());
    view! { <Section title="Hub activity">
        {move || {
            if !history.error.with(String::is_empty) {
                view! { <Alert tone="info">"Hub history could not load. Reload this connection page."</Alert> }.into_any()
            } else if let Some(history) = history.data.get() {
                if history.is_empty() { return view! { <Empty title="No hub turns yet" /> }.into_any(); }
                view! { <div class="k-card">{history.into_iter().map(|row| view! {
                    <Row title=row.agent_name icon="bot" meta=Box::new(move || view! {
                        <Badge>{row.state}</Badge><span>{format!("Requested by {}", row.requester_account_id)}</span>
                        <span>{format!("{} / {}", row.project_id, row.section_id)}</span>
                    }.into_any())>{row.text}</Row>
                }).collect_view()}</div> }.into_any()
            } else { view! { <p role="status">"Loading hub history..."</p> }.into_any() }
        }}
    </Section> }
}
