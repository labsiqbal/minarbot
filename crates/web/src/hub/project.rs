mod room;

use super::*;
use crate::system::kit::{named_confirmation, Confirm};
use wasm_bindgen::{closure::Closure, JsCast};
use web_sys::{EventSource, MessageEvent};

#[derive(Clone, Deserialize, PartialEq)]
struct ProjectName {
    name: String,
}
#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Participant {
    id: String,
    agent_id: String,
    name: String,
    owner_account_id: String,
    owner_name: String,
    node_status: String,
    state: String,
    revision: i64,
    policy: Policy,
    policy_digest: String,
    project_consent_revision: Option<i64>,
}
#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Policy {
    section_ids: Vec<String>,
    invocation: Invocation,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Invocation {
    mode: String,
}
impl Participant {
    fn invocation(&self) -> &'static str {
        match self.policy.invocation.mode.as_str() {
            "owner" => "Owner only",
            "members" => "Specific members",
            _ => "All members",
        }
    }
    fn online(&self) -> &'static str {
        if self.node_status == "ready" {
            "Online"
        } else {
            "Offline"
        }
    }
}

#[derive(Clone, Copy)]
struct ProjectPage {
    actor: StoredValue<Session>,
    data: RwSignal<Option<ProjectName>>,
    sections: RwSignal<Vec<NamedSection>>,
    members: RwSignal<Vec<Member>>,
    participants: RwSignal<Vec<Participant>>,
    error: RwSignal<String>,
    access: RwSignal<bool>,
    busy: RwSignal<bool>,
    refresh: RwSignal<u64>,
    selected: RwSignal<String>,
    confirm: Confirm,
}

impl ProjectPage {
    fn refresh(self) {
        self.refresh.update(|revision| *revision += 1);
    }

    async fn read(self, revision: u64) {
        let result = async {
            let project = read::<ProjectName>("/api/project").await?;
            let sections = read::<Vec<NamedSection>>("/api/sections").await?;
            let members = read::<Vec<Member>>("/api/members").await?;
            let participants = read::<Vec<Participant>>("/api/participants").await?;
            Ok::<_, Failure>((project, sections, members, participants))
        }
        .await;
        if self.refresh.try_get_untracked() != Some(revision) || !self.access.get_untracked() {
            return;
        }
        match result {
            Ok((project, sections, members, participants)) => {
                self.data
                    .maybe_update(|current| replace_if_changed(current, Some(project)));
                self.sections
                    .maybe_update(|current| replace_if_changed(current, sections));
                self.members
                    .maybe_update(|current| replace_if_changed(current, members));
                self.participants
                    .maybe_update(|current| replace_if_changed(current, participants));
            }
            Err(failure) => self.failed(failure),
        }
    }

    fn failed(self, failure: Failure) {
        if failure.access_ended() {
            self.access.try_set(false);
            self.error
                .try_set("Project access ended. Draft saved.".into());
        } else {
            self.error.try_set(failure.message);
        }
    }

    fn change(self, path: &'static str, body: Value, success: impl FnOnce(Value) + 'static) {
        if self.busy.get_untracked() {
            return;
        }
        self.busy.set(true);
        self.error.set(String::new());
        let actor = self.actor.get_value();
        spawn_local(async move {
            let result = actor.post::<Value>(path, body).await;
            if self.busy.try_get_untracked().is_none() {
                return;
            }
            match result {
                Ok(value) => {
                    success(value);
                    self.refresh();
                }
                Err(failure) if failure.status == 401 => self.failed(failure),
                Err(failure) => self.error.set(failure.message),
            }
            self.busy.set(false);
        });
    }

    fn section(self) -> Option<NamedSection> {
        self.sections.with(|sections| {
            self.selected.with(|selected| {
                selected
                    .strip_prefix("#section=")
                    .and_then(|id| sections.iter().find(|section| section.id == id))
                    .or_else(|| sections.first())
                    .cloned()
            })
        })
    }
}

fn replace_if_changed<T: PartialEq>(current: &mut T, next: T) -> bool {
    if *current == next {
        return false;
    }
    *current = next;
    true
}

fn listen(
    path: &str,
    callbacks: impl FnOnce(&EventSource) -> Vec<(&'static str, Closure<dyn FnMut(MessageEvent)>)>,
    on_error: impl FnMut() + 'static,
    on_open: impl FnMut() + 'static,
) {
    let Ok(source) = EventSource::new(path) else {
        return;
    };
    let listeners = callbacks(&source);
    for (event, callback) in &listeners {
        let _ = source.add_event_listener_with_callback(event, callback.as_ref().unchecked_ref());
    }
    let error = Closure::<dyn FnMut()>::new(on_error);
    let open = Closure::<dyn FnMut()>::new(on_open);
    source.set_onerror(Some(error.as_ref().unchecked_ref()));
    source.set_onopen(Some(open.as_ref().unchecked_ref()));
    let handle = StoredValue::new_local((source, listeners, error, open));
    on_cleanup(move || {
        handle.with_value(|(source, listeners, _, _)| {
            source.set_onerror(None);
            source.set_onopen(None);
            for (event, callback) in listeners {
                let _ = source
                    .remove_event_listener_with_callback(event, callback.as_ref().unchecked_ref());
            }
            source.close();
        });
    });
}

pub(super) fn page(
    actor: Session,
    config: StoredValue<Config>,
    error: RwSignal<String>,
) -> impl IntoView {
    let (confirm, confirmation) = named_confirmation("hub-confirmation");
    let page = ProjectPage {
        actor: StoredValue::new(actor.clone()),
        data: RwSignal::new(None),
        sections: RwSignal::new(Vec::new()),
        members: RwSignal::new(Vec::new()),
        participants: RwSignal::new(Vec::new()),
        error,
        access: RwSignal::new(true),
        busy: RwSignal::new(false),
        refresh: RwSignal::new(0),
        selected: RwSignal::new(hash_fragment()),
        confirm,
    };
    let changed = window_event_listener(leptos::ev::hashchange, move |_| {
        page.selected.set(hash_fragment())
    });
    on_cleanup(move || changed.remove());
    if let Ok(timer) =
        set_interval_with_handle(move || page.refresh(), std::time::Duration::from_secs(3))
    {
        on_cleanup(move || timer.clear());
    }
    Effect::new(move |_| {
        let revision = page.refresh.get();
        spawn_local(page.read(revision));
    });
    let section_dialog = NodeRef::new();
    let manager = actor.manager();
    let role = actor.role.clone().unwrap_or_default();
    let accounts_href = config.with_value(|cfg| format!("https://{}/", cfg.accounts_host));
    let submit = move |event: web_sys::SubmitEvent| {
        let Some(data) = form(&event) else {
            return;
        };
        page.change(
            "/api/sections",
            json!({"name":field(&data, "name")}),
            move |value| {
                close_dialog(section_dialog);
                if let Some(section) = value["sectionId"].as_str() {
                    let _ = window().location().set_hash(&format!("section={section}"));
                }
            },
        );
    };
    view! {
        {confirmation}
        <div class="hub-shell bs-root">
            <aside class="hub-sidebar">
                <a class="hub-link" href=accounts_href>{brand()}</a>
                <Show when=move || page.access.get()>
                    <nav class="hub-project-nav" aria-label="Project">
                        <div class="hub-nav-heading">{move || page.data.with(|project| project.as_ref().map(|p| p.name.clone()).unwrap_or_else(|| "Project".into()))}</div>
                        <a class="hub-nav-item" href="#members" aria-current=move || (page.selected.get() == "#members").then_some("page")><Icon name="users" />"Members"</a>
                        <a class="hub-nav-item" href="#agents" aria-current=move || (page.selected.get() == "#agents").then_some("page")><Icon name="bot" />"Agents"</a>
                        <p class="hub-nav-heading">"Sections"</p>
                        {move || page.sections.get().into_iter().map(|section| {
                            let id = section.id.clone();
                            view! { <a class="hub-nav-item" href=format!("#section={}",section.id)
                                aria-current=move || (!matches!(page.selected.get().as_str(), "#members" | "#agents") && page.section().is_some_and(|section| section.id == id)).then_some("page")
                            ><Icon name="hash" />{section.name}</a> }
                        }).collect_view()}
                    </nav>
                </Show>
                {account_footer(actor, error)}
            </aside>
            <main class="hub-main">
                <Show when=move || page.access.get() fallback=move || view! { <div class="k-page"><Header title=Signal::derive(|| String::from("Project unavailable")) />{alert(error)}</div> }>
                    <ProjectStream page />
                    <header class="hub-header"><h1><Icon name="folder" />{move || page.data.with(|project| project.as_ref().map(|p| p.name.clone()).unwrap_or_else(|| String::from("Project")))}</h1>
                        <div class="hub-actions">{badge(role.clone())}
                            {manager.then(|| view! { <button class="k-icon-btn" type="button" aria-label="Create section" on:click=move |_| dialog(section_dialog)><Icon name="plus" /></button> })}
                            <button class="k-icon-btn" type="button" aria-label="Reload project" on:click=move |_| page.refresh()><Icon name="refresh" /></button>
                        </div>
                    </header>
                    {alert(error)}
                    <Show when=move || page.data.get().is_some() fallback=move || view! { <p class="hub-notice" role="status" hidden=move || !error.with(String::is_empty)>"Loading project…"</p> }>
                        <Show when=move || page.selected.get() == "#members"><Members page /></Show>
                        <Show when=move || page.selected.get() == "#agents"><Participants page /></Show>
                        <Show when=move || !matches!(page.selected.get().as_str(), "#members" | "#agents")>
                            <For each=move || page.section() key=|section| section.id.clone() children=move |section| view! { <room::Room page section /> } />
                        </Show>
                    </Show>
                    <dialog class="k-dialog hub-dialog" node_ref=section_dialog aria-labelledby="hub-section-title">
                        <h2 id="hub-section-title">"Create section"</h2>
                        <form class="hub-form" method="post" action="/api/sections" on:submit=submit>
                            <label class="k-field"><span class="k-field__label">"Section name"</span><input class="k-input" name="name" required maxlength="120" autofocus /></label>
                            <div class="hub-actions"><button class=BUTTON type="button" disabled=page.busy on:click=move |_| close_dialog(section_dialog)>"Cancel"</button><button class=PRIMARY type="submit" disabled=page.busy>"Create section"</button></div>
                        </form>
                    </dialog>
                </Show>
            </main>
        </div>
    }
}

#[component]
fn ProjectStream(page: ProjectPage) -> impl IntoView {
    let checking = RwSignal::new(false);
    listen(
        "/api/events",
        |_| {
            vec![(
                "authority",
                Closure::new(move |_: MessageEvent| page.refresh()),
            )]
        },
        move || {
            if checking.get_untracked() {
                return;
            }
            checking.set(true);
            spawn_local(async move {
                if let Err(failure) = read::<Session>("/api/session").await {
                    if failure.access_ended() {
                        page.failed(failure);
                    }
                }
                checking.try_set(false);
            });
        },
        move || page.refresh(),
    );
}

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Invitation {
    id: String,
    role: String,
    state: String,
    expires_at: f64,
}

#[component]
fn Members(page: ProjectPage) -> impl IntoView {
    let actor = page.actor.get_value();
    let owner = actor.role.as_deref() == Some("owner");
    let manager = actor.manager();
    let invites = RwSignal::new(None::<Vec<Invitation>>);
    let invite_dialog = NodeRef::new();
    let link = RwSignal::new(None::<String>);
    if manager {
        Effect::new(move |_| {
            let revision = page.refresh.get();
            spawn_local(async move {
                let result = read::<Vec<Invitation>>("/api/invitations").await;
                if page.refresh.try_get_untracked() != Some(revision)
                    || invites.try_get_untracked().is_none()
                {
                    return;
                }
                match result {
                    Ok(rows) => invites.set(Some(rows)),
                    Err(failure) => page.failed(failure),
                }
            });
        });
    }
    let submit = move |event: web_sys::SubmitEvent| {
        let Some(data) = form(&event) else {
            return;
        };
        page.change(
            "/api/invitations",
            json!({"mailbox":field(&data,"mailbox"),"role":field(&data, "role")}),
            move |result| {
                link.set(result["url"].as_str().map(str::to_owned));
            },
        );
    };
    view! { <div class="k-page bs-root">
        <Header title=Signal::derive(|| String::from("Members")) actions=Box::new(move || {
            manager.then(|| view! { <button class=PRIMARY type="button" on:click=move |_| {
                link.set(None);
                dialog(invite_dialog);
            }><Icon name="plus" />"Invite member"</button> }).into_any()
        })/>
        <div class="k-card">{move || page.members.get().into_iter().map(|member| {
            let can_edit = manager && member.role != "owner" && (owner || member.role != "admin");
            let role = member.role.clone();
            let title = member.name.clone();
            view! { <Row title=title icon="users" meta=Box::new(move || badge(role).into_any())
                trailing=Box::new(move || can_edit.then(|| member_actions(page, member, owner)).into_any()) /> }
        }).collect_view()}</div>
        {manager.then(|| view! { <Section title="Invitations">
            {move || match invites.get() {
                None => view! { <crate::connection_ui::Loading label="Loading invitations…" /> }.into_any(),
                Some(rows) if rows.is_empty() => view! { <crate::connection_ui::Empty title="No invitations" /> }.into_any(),
                Some(rows) => view! { <div class="k-card">{rows.into_iter().map(|invite| {
                    let short_id = invite.id.chars().take(8).collect::<String>();
                    let title = format!("Invitation {short_id}");
                    let expires = js_sys::Date::new(&invite.expires_at.into()).to_locale_string("en", &wasm_bindgen::JsValue::UNDEFINED).as_string().unwrap_or_default();
                    let pending = invite.state == "pending" && (owner || invite.role != "admin");
                    view! { <Row title=title
                        meta=Box::new(move || view! { {badge(invite.state)}<span>{invite.role}</span><span>{format!("Expires {expires}")}</span> }.into_any())
                        trailing=Box::new(move || pending.then(|| invitation_actions(page, invite.id, short_id)).into_any()) /> }
                }).collect_view()}</div> }.into_any(),
            }}
        </Section> })}
        <dialog class="hub-form-dialog" node_ref=invite_dialog aria-labelledby="hub-invite-title"
            on:close=move |_| link.set(None) on:cancel=move |event: web_sys::Event| { if page.busy.get_untracked() { event.prevent_default(); } }>
          <div class="k-dialog k-dialog--md bs-root">
            <div class="k-dialog__head">
                <h2 class="k-dialog__title" id="hub-invite-title">{move || if link.get().is_some() { "Invitation ready" } else { "Invite member" }}</h2>
                <button class="k-icon-btn" type="button" aria-label="Close" disabled=page.busy on:click=move |_| close_dialog(invite_dialog)><Icon name="close" /></button>
            </div>
            <div class="k-dialog__body"><Show when=move || link.get().is_some() fallback=move || view! {
                <form id="hub-invite" class="hub-form" method="post" action="/api/invitations" on:submit=submit>
                    <label class="k-field"><span class="k-field__label">"Email"</span><input class="k-input" name="mailbox" type="email" required maxlength="254" autofocus /></label>
                    <label class="k-field"><span class="k-field__label">"Role"</span><span class="hub-dialog-select">
                        <select class="k-input k-select" name="role">
                            {manager_roles(owner).map(|role| view! { <option value=role>{match role { "admin" => "Admin", "viewer" => "Viewer", _ => "Member" }}</option> }).collect_view()}
                        </select><span class="k-select__chev"><Icon name="chevron-down" /></span>
                    </span></label>
                </form>
            }>
                <div class="k-field"><label class="k-field__label" for="hub-invite-link">"Private invitation link"</label>
                    <textarea id="hub-invite-link" class="k-input k-textarea" rows="4" readonly
                        on:focus=move |event| { event_target::<web_sys::HtmlTextAreaElement>(&event).select(); }
                        prop:value=move || link.get().unwrap_or_default() aria-describedby="hub-invite-hint"></textarea>
                    <span id="hub-invite-hint" class="k-field__hint">"Send this link privately to the invited email address. Shown once."</span>
                </div>
            </Show></div>
            <div class="k-dialog__foot"><Show when=move || link.get().is_some() fallback=move || view! {
                <button class=PRIMARY type="submit" form="hub-invite" disabled=page.busy>"Create invitation"</button>
            }><button class=BUTTON type="button" on:click=move |_| close_dialog(invite_dialog)>"Done"</button></Show></div>
          </div>
        </dialog>
    </div> }
}

fn invitation_actions(page: ProjectPage, id: String, short_id: String) -> impl IntoView {
    view! {
        <details class="hub-menu">
            <summary class="k-icon-btn" aria-label=format!("Manage invitation {short_id}")><Icon name="ellipsis" /></summary>
            <div class="k-pop hub-menu-items">
                <button class="k-menu__item is-danger" type="button" disabled=page.busy on:click=move |_| {
                    let id = id.clone();
                    crate::system::kit::ask(Some(page.confirm), "Revoke invitation?", "This invitation link will stop working.", "Revoke", true, move || {
                        page.change("/api/invitations/revoke",json!({"invitationId":id}), |_| ());
                    });
                }>"Revoke invitation"</button>
            </div>
        </details>
    }
}

fn member_actions(page: ProjectPage, member: Member, owner: bool) -> impl IntoView {
    let name = member.name;
    let target = member.account_id;
    let rev = member.revision;
    let remove = {
        let target = target.clone();
        let name = name.clone();
        move |_| {
            let body = json!({"accountId":target,"revision":rev,"role":"revoked"});
            crate::system::kit::ask(
                Some(page.confirm),
                format!("Remove {name}?"),
                "This closes their project streams, revokes their agents, and blocks new turns.",
                "Remove member",
                true,
                move || {
                    page.change("/api/members", body.clone(), |_| ());
                },
            );
        }
    };
    view! { <details class="hub-menu">
        <summary class="k-icon-btn" aria-label=format!("Manage {name}")><Icon name="ellipsis" /></summary>
        <div class="k-pop hub-menu-items">
            {manager_roles(owner).filter(|role| *role != member.role).map(|role| {
                let target = target.clone();
                view! { <button class="k-menu__item" type="button" disabled=page.busy on:click=move |_| {
                    page.change("/api/members",json!({"accountId":target,"revision":rev,"role":role}), |_| ());
                }>{format!("Make {role}")}</button> }
            }).collect_view()}
            <button class="k-menu__item is-danger" type="button" disabled=page.busy on:click=remove>"Remove member"</button>
        </div>
    </details> }
}

#[component]
fn Participants(page: ProjectPage) -> impl IntoView {
    let actor = page.actor.get_value();
    let manager = actor.manager();
    let account = StoredValue::new(actor.account_id);
    view! { <div class="k-page bs-root">
        <Header title=Signal::derive(|| String::from("Agents")) />
        <Section title="Participating agents">
            {move || {
                let rows = page.participants.get();
                if rows.is_empty() {
                    return view! { <p>"No agents yet. Join from your local Minarbot app."</p> }.into_any();
                }
                view! { <div class="k-card">{rows.into_iter().map(|agent| {
                    let may_approve = manager && agent.state == "proposed" && agent.project_consent_revision != Some(agent.revision);
                    let may_withdraw = manager || account.with_value(|id| *id == agent.owner_account_id);
                    let title = agent.name.clone();
                    let owner = agent.owner_name.clone();
                    let invocation = agent.invocation();
                    let online = agent.online();
                    let status = if agent.state == "proposed" { "Awaiting approval".into() } else { agent.state.clone() };
                    view! { <Row title=title icon="bot"
                        meta=Box::new(move || view! { <span>{owner}</span>{badge(online.into())}{badge(invocation.into())}{badge(status)} }.into_any())
                        trailing=Box::new(move || {
                            let approval = json!({"participationId":agent.id,"revision":agent.revision,"policyDigest":agent.policy_digest});
                            let withdrawal = json!({"participationId":agent.id});
                            let approve_name = agent.name.clone();
                            let owner = agent.owner_name.clone();
                            view! {
                                {may_approve.then(|| view! { <button class=BUTTON type="button" disabled=page.busy on:click=move |_| {
                                    let body = approval.clone();
                                    crate::system::kit::ask(Some(page.confirm), format!("Approve {approve_name}?"),
                                        format!("{owner} owns this agent. Invocation: {invocation}. The owner must separately approve locally."),
                                        "Approve agent", false, move || { page.change("/api/participants/consent",body.clone(), |_| ()); });
                                }>"Approve agent"</button> })}
                                {may_withdraw.then(|| view! { <button class=BUTTON type="button" disabled=page.busy on:click=move |_| {
                                    let body = withdrawal.clone();
                                    crate::system::kit::ask(Some(page.confirm), format!("Withdraw {}?", agent.name), "Stops new turns and ends current authorization in this project.", "Withdraw", true, move || {
                                        page.change("/api/participants/withdraw",body.clone(), |_| ());
                                    });
                                }>"Withdraw participation"</button> })}
                            }.into_any()
                        }) />
                    }
                }).collect_view()}</div> }.into_any()
            }}
        </Section>
    </div> }
}
