use crate::{
    connection_ui::*,
    settings_recovery::{Command, Journal, Recovery, UUID},
};
use leptos::prelude::*;
use serde::Deserialize;
use serde_json::json;

const PATH: &str = "/api/connected-apps";
const KEY: &str = "/api/connected-apps/key";
const KEY_WARNING: &str = "Connected accounts, agent access, pending approvals, and saved \"Always allow\" choices will be reset. Existing accounts remain in the old Composio project.";

#[derive(Clone, Deserialize, PartialEq)]
struct KeyStatus {
    source: String,
    revision: u64,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Account {
    id: String,
    toolkit: String,
    status: String,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Toolkit {
    slug: String,
    name: String,
    #[serde(rename = "managedAuth")]
    managed_auth: bool,
}
#[derive(Clone, Deserialize, PartialEq)]
pub(crate) struct Catalog {
    configured: bool,
    items: Vec<Toolkit>,
    categories: Vec<Category>,
    next_after: Option<String>,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Category {
    id: String,
    name: String,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Items<T> {
    items: Vec<T>,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Grant {
    toolkit: String,
    enabled: bool,
    revision: u64,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Policy {
    toolkit: String,
    tool: String,
    account: String,
    fingerprint: String,
}

#[derive(Clone, Copy)]
struct Apps {
    key: Query<KeyStatus>,
    catalog: Query<Catalog>,
    accounts: Query<Items<Account>>,
    grants: Query<Items<Grant>>,
    policies: Query<Items<Policy>>,
    agent: RwSignal<String>,
    agents: Signal<Vec<(String, String)>>,
    project: Option<ProjectApps>,
    selected: RwSignal<String>,
    search: RwSignal<String>,
    query: RwSignal<String>,
    category: RwSignal<String>,
    cursor: Cursor,
    setup: RwSignal<bool>,
    replacing: RwSignal<bool>,
    secret: RwSignal<String>,
    link: RwSignal<Option<String>>,
    journal: Journal,
    confirmation: Confirm,
}

#[derive(Clone, Copy)]
pub(crate) struct ProjectApps {
    pub id: StoredValue<String>,
    pub members: StoredValue<Vec<String>>,
    pub inherited: bool,
    pub change: Callback<bool>,
    pub locked: Signal<bool>,
    pub catalog: StoredValue<Option<Catalog>>,
}

fn valid(command: &Command) -> bool {
    let route = format!("^/api/(connected-apps/(key(/remove)?|connect/[a-z0-9_-]{{1,100}}|grant-all/[a-z0-9_-]{{1,100}}|disconnect/[a-z0-9_-]{{1,160}})|agents/{UUID}/(connected-apps/[a-z0-9_-]{{1,100}}|connected-app-policies)|(agents|channels|threads)/{UUID}/turns/{UUID}/app-approvals/[^/]{{1,500}})$");
    if !command.valid(
        &route,
        &[
            "revision",
            "enabled",
            "fingerprint",
            "decision",
            "confirmed",
        ],
    ) {
        return false;
    }
    let revision = command.body["revision"]
        .as_u64()
        .filter(|value| *value < (1 << 53));
    if matches!(
        command.path.as_str(),
        KEY | "/api/connected-apps/key/remove"
    ) {
        revision.is_some() && command.valid("^/api/connected-apps/key(/remove)?$", &["revision"])
    } else if command.path.contains("/app-approvals/") {
        revision.is_some_and(|value| value > 0)
            && matches!(
                command.body["decision"].as_str(),
                Some("allow" | "always" | "deny")
            )
    } else if command.path.starts_with("/api/agents/") && command.path.contains("/connected-apps/")
    {
        revision.is_some() && command.body["enabled"].is_boolean()
    } else if command.path.ends_with("/connected-app-policies") {
        command.body["fingerprint"].as_str().is_some_and(|value| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    } else {
        true
    }
}

impl Apps {
    fn refresh(self) {
        self.key.reload.run(());
        self.catalog.reload.run(());
        self.accounts.reload.run(());
        self.grants.reload.run(());
        self.policies.reload.run(());
    }
    fn title(self, slug: &str) -> String {
        self.catalog
            .data
            .with(|data| {
                data.as_ref()
                    .and_then(|data| data.items.iter().find(|app| app.slug == slug))
                    .map(|app| app.name.clone())
            })
            .unwrap_or_else(|| {
                match slug {
                    "gmail" => "Gmail",
                    "slack" => "Slack",
                    "github" => "GitHub",
                    "googlecalendar" => "Google Calendar",
                    _ => slug,
                }
                .into()
            })
    }
    fn needs_key(self) -> bool {
        self.journal
            .pending
            .with(|command| command.as_ref().is_some_and(|command| command.path == KEY))
    }
    fn execute(self, command: Command) {
        if self.journal.busy.get_untracked() {
            return;
        }
        let is_key = command.path == KEY;
        let key_change = command.path.starts_with(KEY);
        let key = is_key.then(|| self.secret.get_untracked());
        if key.as_ref().is_some_and(|key| key.trim().is_empty()) {
            self.journal
                .error
                .set("Paste the Composio API key again to retry this save.".into());
            return;
        }
        let popup = if command.path.contains("/connect/") {
            window()
                .open_with_url_and_target("about:blank", "_blank")
                .ok()
                .flatten()
                .inspect(|popup| {
                    let _ = popup.set_opener(&wasm_bindgen::JsValue::NULL);
                })
        } else {
            None
        };
        self.journal.run(command, key, move |result| {
            let redirect = result
                .as_ref()
                .and_then(|result| result["redirect_url"].as_str())
                .and_then(external_url);
            if let Some(popup) = &popup {
                if let Some(redirect) = &redirect {
                    let _ = popup.location().set_href(redirect);
                } else {
                    let _ = popup.close();
                }
            }
            if result.is_none() {
                return;
            }
            if key_change {
                self.secret.set(String::new());
                self.replacing.set(false);
                self.setup.set(false);
            }
            if key_change || redirect.is_some() {
                self.link.set(redirect);
            }
            self.refresh();
        });
    }
    fn open_setup(self) {
        if self.journal.locked() {
            return;
        }
        let replace = self
            .key
            .data
            .with_untracked(|row| row.as_ref().is_some_and(|row| row.source == "settings"));
        let open = UiAction::new(move |()| {
            self.replacing.set(replace);
            self.setup.set(true);
        });
        if replace {
            confirm(
                self.confirmation,
                "Replace Composio API key?",
                KEY_WARNING,
                "Replace key",
                true,
                open,
            );
        } else {
            open.run(());
        }
    }
    fn save_key(self) {
        if let Some(command) = self.journal.pending.get_untracked() {
            if command.path == KEY {
                self.execute(command);
            }
        } else if let Some(status) = self.key.data.get_untracked() {
            if status.source != "environment" && !self.key.busy.get_untracked() {
                self.execute(Command::new(KEY, json!({ "revision": status.revision })));
            }
        }
    }
    fn integration_menu(self, status: Option<KeyStatus>) -> impl IntoView {
        let saved = status
            .as_ref()
            .is_some_and(|status| status.source == "settings");
        let allowed = status.is_some();
        let mut items = vec![MenuItem::new(
            if saved { "Replace key" } else { "Set up" },
            UiAction::new(move |()| self.open_setup()),
        )];
        if let Some(key) = status.filter(|_| saved) {
            items.push(
                MenuItem::new(
                    "Remove key",
                    UiAction::new(move |()| {
                        let revision = key.revision;
                        confirm(
                            self.confirmation,
                            "Remove Composio API key?",
                            KEY_WARNING,
                            "Remove key",
                            true,
                            UiAction::new(move |()| {
                                self.execute(Command::new(
                                    format!("{KEY}/remove"),
                                    json!({ "revision": revision }),
                                ));
                            }),
                        );
                    }),
                )
                .danger(),
            );
        }
        view! { <Menu label="Integration setup" items=items disabled=move || self.journal.locked() || !allowed /> }
    }
}

pub fn panel() -> impl IntoView {
    scoped_panel(None)
}

pub(crate) fn scoped_panel(project: Option<ProjectApps>) -> impl IntoView {
    let agents = scoped_agents(project.is_none(), project.map(|project| project.members));
    let agent = selected_agent(agents);
    let journal = match project {
        Some(project) => Journal::new(
            project
                .id
                .with_value(|id| format!("minarbot:connected-app-command:{id}")),
            4000,
            |command| {
                valid(command)
                    && !command.path.starts_with(KEY)
                    && !command.path.contains("/app-approvals/")
            },
        )
        .for_project(Some(project.id)),
        None => Journal::new("minarbot:connected-app-command", 4000, valid),
    };
    let cursor = Cursor::new(Vec::new());
    let query_text = RwSignal::new(String::new());
    let category = RwSignal::new(String::new());
    let catalog = cached_query(
        move || {
            let after = cursor.with_untracked(|rows| rows.last().cloned().unwrap_or_default());
            format!(
                "{}?search={}&category={}&after={}",
                journal.endpoint(PATH),
                encode(&query_text.get_untracked()),
                encode(&category.get_untracked()),
                encode(&after)
            )
        },
        project.map(|project| project.catalog),
    );
    let apps = Apps {
        key: query(|| KEY.into()),
        catalog,
        accounts: query(move || journal.endpoint(&format!("{PATH}/accounts"))),
        grants: query(move || {
            let id = agent.get_untracked();
            if id.is_empty() {
                String::new()
            } else {
                journal.endpoint(&format!("/api/agents/{}/connected-apps", encode(&id)))
            }
        }),
        policies: query(move || {
            let id = agent.get_untracked();
            if id.is_empty() {
                String::new()
            } else {
                journal.endpoint(&format!(
                    "/api/agents/{}/connected-app-policies",
                    encode(&id)
                ))
            }
        }),
        agent,
        agents,
        project,
        cursor,
        category,
        query: query_text,
        selected: RwSignal::new(String::new()),
        search: RwSignal::new(String::new()),
        setup: RwSignal::new(false),
        replacing: RwSignal::new(false),
        secret: RwSignal::new(String::new()),
        link: RwSignal::new(None),
        journal,
        confirmation: Confirm::new(None),
    };
    Effect::new(move |_| {
        let _ = apps.agent.get();
        apps.grants.data.set(None);
        apps.policies.data.set(None);
        apps.grants.reload.run(());
        apps.policies.reload.run(());
    });
    Effect::new(move |_| {
        let _ = apps.selected.get();
        request_animation_frame(move || {
            if let Ok(Some(heading)) = document().query_selector(".bs-set3-apps h1") {
                if let Some(heading) =
                    wasm_bindgen::JsCast::dyn_ref::<web_sys::HtmlElement>(&heading)
                {
                    let _ = heading.focus();
                }
            }
        });
    });
    poll(15, move || {
        if !apps.journal.busy.get_untracked() {
            apps.refresh();
        }
    });
    let refresh = UiAction::new(move |()| apps.refresh());
    let retry = UiAction::new(move |()| {
        if let Some(command) = apps.journal.pending.get_untracked() {
            apps.execute(command);
        }
    });
    let enter_key = UiAction::new(move |()| apps.setup.set(true));
    let error = Signal::derive(move || {
        [
            apps.journal.error.get(),
            apps.key.error.get(),
            apps.catalog.error.get(),
            apps.accounts.error.get(),
            apps.grants.error.get(),
            apps.policies.error.get(),
        ]
        .into_iter()
        .find(|message| !message.is_empty())
        .unwrap_or_default()
    });
    view! {
        <div class="k-page bs-root bs-set3-apps">
            <Show when=move || !apps.selected.with(String::is_empty)>
                <Button label="Connected apps" aria="Back to Connected apps" variant="ghost" icon="arrow-left"
                    click=UiAction::new(move |()| apps.selected.set(String::new())) />
            </Show>
            <Header title=move || apps.selected.with(|slug| if slug.is_empty() { "Connected apps".into() } else { apps.title(slug) })
                actions=Box::new(move || view! { {move || project.is_none().then(|| apps.integration_menu(apps.key.data.get()))} }.into_any()) />
            <Show when=move || apps.selected.with(String::is_empty)>
                {project.is_none().then(|| view! {
                    {crate::messaging_connections::discord()}
                    {crate::messaging_connections::whatsapp()}
                })}
            </Show>
            <ErrorNotice error=error retry=refresh label="Try again" />
            <Recovery journal=apps.journal retry=retry confirmation=apps.confirmation refresh=refresh enter_key=enter_key
                needs_key=move || apps.needs_key() && apps.secret.with(|value| value.trim().is_empty()) />
            {project.map(|project| view! {
                <Show when=move || apps.selected.with(String::is_empty)>
                    <div class="k-card"><Row title="Allow global apps" trailing=Box::new(move || view! {
                        <span title="Member agents may also use apps granted to them in Settings. Project accounts take priority for the same toolkit.">
                            <Switch label="Allow global apps" checked=project.inherited disabled=move || apps.journal.locked() || project.locked.get()
                                change=Callback::new(move |enabled| {
                                    if enabled {
                                        confirm(apps.confirmation, "Allow installation apps in this project?",
                                            "Members can use their installation accounts alongside this project’s accounts. Approval choices stay separate for this project.",
                                            "Allow installation apps", false, UiAction::new(move |()| project.change.run(enabled)));
                                    } else { project.change.run(enabled); }
                                }) />
                        </span>
                    }.into_any()) /></div>
                </Show>
            })}
            <Show when=move || apps.catalog.data.with(|data| data.as_ref().is_some_and(|data| !data.configured))
                fallback=move || view! {
                    <Show when=move || apps.selected.with(String::is_empty) fallback=move || detail(apps)>
                        {move || browse(apps)}
                    </Show>
                }>
                <Empty title="Connect apps" icon="blocks">
                    {if project.is_none() { view! {
                        <Button label="Set up" disabled=move || apps.journal.locked() || apps.key.data.with(Option::is_none)
                            click=UiAction::new(move |()| apps.open_setup()) />
                    }.into_any() } else { view! { <div class="k-empty__text">"Set up Composio in Settings > Connected apps."</div> }.into_any() }}
                </Empty>
            </Show>
            <Show when=move || apps.setup.get()>{move || setup(apps, error, refresh, retry)}</Show>
            <ConfirmDialog state=apps.confirmation />
        </div>
    }
}

fn setup(apps: Apps, error: Signal<String>, refresh: UiAction, retry: UiAction) -> impl IntoView {
    let close = UiAction::new(move |()| {
        apps.setup.set(false);
        if apps.journal.pending.with_untracked(Option::is_none) {
            apps.replacing.set(false);
            apps.secret.set(String::new());
        }
    });
    let environment = move || {
        apps.key
            .data
            .with(|row| row.as_ref().is_some_and(|row| row.source == "environment"))
    };
    view! {
        <Modal title=if apps.replacing.get_untracked() { "Replace key".into() } else { "Set up Composio".into() }
            busy=apps.journal.busy close=close footer=Box::new(move || view! {
                <Show when=move || !environment() || apps.needs_key()>
                    {move || view! {
                        <Button label=if apps.needs_key() { "Try again" } else { "Save key" } variant="primary" submit=true form="composio-key-form"
                            busy=apps.journal.busy disabled=move || apps.journal.invalid.get() || apps.secret.with(|value| value.trim().is_empty()) ||
                                apps.journal.pending.with(Option::is_some) && !apps.needs_key() />
                    }}
                </Show>
            }.into_any())>
            <div class="k-stack">
                <ErrorNotice error=error retry=refresh label="Try again" />
                <Recovery journal=apps.journal retry=retry confirmation=apps.confirmation refresh=refresh
                    needs_key=move || apps.needs_key() && apps.secret.with(|value| value.trim().is_empty()) />
                <Show when=move || environment() && !apps.needs_key() fallback=move || view! {
                    <form id="composio-key-form" on:submit=move |event: web_sys::SubmitEvent| { event.prevent_default(); apps.save_key(); }>
                        <Field label="Composio API key" name="composio-key">
                            <input class="k-input" id="composio-key" name="key" type="password" maxlength="4096" autocomplete="off" spellcheck="false"
                                disabled=move || apps.journal.busy.get() || apps.journal.invalid.get()
                                prop:value=apps.secret on:input=move |event| apps.secret.set(event_target_value(&event)) />
                        </Field>
                    </form>
                }><Badge>"From server environment"</Badge></Show>
                <a href="https://platform.composio.dev/" target="_blank" rel="noreferrer">"Get API key"</a>
            </div>
        </Modal>
    }
}

fn badge(account: Account) -> impl IntoView {
    let active = account.status == "ACTIVE";
    view! { <Badge success=active>{if active { "Connected".into() } else { account.status }}</Badge> }
}

fn browse(apps: Apps) -> impl IntoView {
    let reload = UiAction::new(move |()| apps.catalog.reload.run(()));
    let check = UiAction::new(move |()| apps.refresh());
    view! {
        {move || apps.link.get().map(|url| view! {
            <Alert tone="info" actions=Box::new(move || view! { <Button label="Check connection" size="sm" click=check /> }.into_any())>
                <div class="k-alert__title">"Finish connecting in Composio"</div>
                <a href=url target="_blank" rel="noopener noreferrer">"Open connection page"</a>
            </Alert>
        })}
        <Section title="Accounts">
            {move || match apps.accounts.data.get() {
                None if apps.accounts.error.with(String::is_empty) => view! { <Loading count=2 label="Loading connected apps..." /> }.into_any(),
                None => ().into_any(),
                Some(rows) if rows.items.is_empty() => view! { <Empty title="No connected accounts" /> }.into_any(),
                Some(rows) => view! { <div class="k-card">{rows.items.into_iter().map(|row| {
                    let title = apps.title(&row.toolkit);
                    let slug = row.toolkit.clone();
                    let id = row.id.clone();
                    view! { <Row title=title click=UiAction::new(move |()| apps.selected.set(slug.clone())) meta=Box::new(move || view! { <span>{id}</span>{badge(row)} }.into_any()) /> }
                }).collect_view()}</div> }.into_any(),
            }}
        </Section>
        <Section title="Browse apps">
            <form class="k-inline apps-search" on:submit=move |event: web_sys::SubmitEvent| {
                event.prevent_default();
                apps.cursor.set(Vec::new());
                apps.query.set(apps.search.get_untracked());
                reload.run(());
            }>
                <Field label="Search apps" name="apps-search">
                    <input class="k-input" id="apps-search" name="search" type="search" maxlength="100" placeholder="Gmail, Slack, GitHub..."
                        disabled=move || apps.journal.locked() prop:value=apps.search on:input=move |event| apps.search.set(event_target_value(&event)) />
                </Field>
                <Button label="Search" icon="search" submit=true disabled=move || apps.journal.locked() />
            </form>
            {move || apps.catalog.data.get().map(|catalog| view! {
                <div class="k-row-actions" aria-label="App categories">
                    {std::iter::once(Category { id:String::new(), name:"All apps".into() }).chain(catalog.categories).map(|category| {
                        let id = StoredValue::new(category.id);
                        let active = move || apps.category.with(|category| id.with_value(|id| category == id));
                        view! {
                            <button type="button" class="k-btn k-btn--sm" class:k-btn--brand=active class:k-btn--ghost=move || !active()
                                aria-pressed=move || active().to_string() disabled=move || apps.journal.locked()
                                on:click=move |_| { apps.category.set(id.get_value()); apps.cursor.set(Vec::new()); reload.run(()); }>{category.name}</button>
                        }
                    }).collect_view()}
                </div>
            })}
            {move || match apps.catalog.data.get() {
                None if apps.catalog.error.with(String::is_empty) => view! { <Loading /> }.into_any(),
                None => ().into_any(),
                Some(catalog) if catalog.items.is_empty() => view! { <Empty title="No apps found" /> }.into_any(),
                Some(catalog) => view! { <div class="k-card">{catalog.items.into_iter().map(|row| {
                    let active = apps.accounts.data.with(|rows| rows.as_ref().is_some_and(|rows| rows.items.iter().any(|account| account.toolkit == row.slug && account.status == "ACTIVE")));
                    let slug = StoredValue::new(row.slug);
                    let manage = UiAction::new(move |()| apps.selected.set(slug.get_value()));
                    let connect = UiAction::new(move |()| apps.execute(Command::new(format!("{PATH}/connect/{}", encode(&slug.get_value())), json!({}))));
                    let meta = move || view! { <Badge success=true>"Connected"</Badge> }.into_any();
                    let trailing = move || view! { <Button label=if active { "Manage" } else if row.managed_auth { "Connect" } else { "Custom auth needed" }
                        size="sm" disabled=move || apps.journal.locked() || !active && !row.managed_auth
                            || apps.project.is_some() && (apps.catalog.busy.get() || apps.accounts.busy.get()
                            || !apps.catalog.error.with(String::is_empty) || !apps.accounts.error.with(String::is_empty))
                        click=if active { manage } else { connect } /> }.into_any();
                    if active { view! { <Row title=row.name icon="blocks" click=manage meta=Box::new(meta) trailing=Box::new(trailing) /> }.into_any() }
                    else { view! { <Row title=row.name icon="blocks" trailing=Box::new(trailing) /> }.into_any() }
                }).collect_view()}</div> }.into_any(),
            }}
            <Pager cursor=apps.cursor next=Signal::derive(move || apps.catalog.data.get().and_then(|catalog| catalog.next_after))
                previous_label="Previous apps" next_label="Next apps" refresh=reload disabled=move || apps.journal.locked() />
        </Section>
    }
}

fn detail(apps: Apps) -> impl IntoView {
    let slug = apps.selected.get();
    let toolkit = StoredValue::new(slug);
    let connected = {
        move || {
            apps.accounts.data.with(|data| {
                data.as_ref().is_some_and(|data| {
                    data.items.iter().any(|row| {
                        toolkit.with_value(|slug| &row.toolkit == slug) && row.status == "ACTIVE"
                    })
                })
            })
        }
    };
    view! {
        <Section title="Accounts">
            {move || {
                let Some(rows) = apps.accounts.data.get() else {
                    return view! { <Loading count=1 /> }.into_any();
                };
                let rows = rows.items.into_iter().filter(|row| toolkit.with_value(|slug| slug == &row.toolkit)).collect::<Vec<_>>();
                if rows.is_empty() { return view! { <Empty title="No connected accounts" /> }.into_any(); }
                view! { <div class="k-card">{rows.into_iter().map(|row| {
                    let id = row.id.clone();
                    let label = format!("Manage account {id}");
                    let title = apps.title(&row.toolkit);
                    let remove = UiAction::new(move |()| {
                        let path = format!("{PATH}/disconnect/{}", encode(&id));
                        confirm(apps.confirmation, format!("Disconnect {title}?"), "Agent access is revoked. Completed app actions cannot be undone.",
                            "Disconnect", true, UiAction::new(move |()| apps.execute(Command::new(path.clone(), json!({})))));
                    });
                    view! { <Row title=row.id.clone() meta=Box::new(move || badge(row).into_any()) trailing=Box::new(move || view! {
                        <Menu label=label disabled=move || apps.journal.locked() items=vec![MenuItem::new("Disconnect", remove).danger()] />
                    }.into_any()) /> }
                }).collect_view()}</div> }.into_any()
            }}
        </Section>
        <Section title="Agent access" actions=Box::new(move || view! {
            <Show when=move || !apps.agents.with(Vec::is_empty)>
                <Select name="apps-agent" label="Agent for connected apps" small=true value=apps.agent.into()
                    options=apps.agents disabled=move || apps.journal.locked()
                    change=Callback::new(move |value| apps.agent.set(value)) />
            </Show>
        }.into_any())>
            {move || {
                let id = apps.agent.get();
                if id.is_empty() { return view! { <Empty title="No agents yet" /> }.into_any(); }
                let Some(grants) = apps.grants.data.get() else {
                    return view! { <Loading count=1 /> }.into_any();
                };
                if !connected() { return view! { <Empty title="No connected account" /> }.into_any(); }
                let title = apps.title(&toolkit.get_value());
                let agent = apps.agents.get().into_iter().find(|(agent, _)| agent == &id).map_or_else(String::new, |(_, name)| name);
                let grant = grants.items.into_iter().find(|row| toolkit.with_value(|slug| slug == &row.toolkit));
                let revision = grant.as_ref().map_or(0, |grant| grant.revision);
                let enabled = grant.is_some_and(|grant| grant.enabled);
                view! { <div class="k-card"><Row title=title.clone() trailing=Box::new(move || view! {
                    <Switch label=format!("Allow {agent} to use {title}") checked=enabled disabled=move || apps.journal.locked() || apps.grants.busy.get()
                        change=Callback::new(move |enabled| apps.execute(Command::new(
                            format!("/api/agents/{}/connected-apps/{}", encode(&id), encode(&toolkit.get_value())),
                            json!({ "revision": revision, "enabled": enabled })
                        ))) />
                }.into_any()) /></div> }.into_any()
            }}
            {apps.project.map(|project| view! {
                <Show when=connected>
                    <Button label="Allow all members" size="sm" disabled=move || apps.journal.locked() || apps.agents.with(Vec::is_empty)
                        click=UiAction::new(move |()| {
                            let slug = toolkit.get_value();
                            let title = apps.title(&slug);
                            let count = project.members.with_value(Vec::len);
                            confirm(apps.confirmation, format!("Allow all project members to use {title}?"),
                                format!("This grants {count} current members access. Actions that write or have unknown effects still ask for approval in chat."),
                                "Allow all members", false, UiAction::new(move |()| apps.execute(Command::new(
                                    format!("{PATH}/grant-all/{}", encode(&slug)), json!({ "confirmed": true })
                                ))));
                        }) />
                </Show>
            })}
        </Section>
        <Section title="Allowed actions">
            {move || {
                let id = apps.agent.get();
                let rows = apps.policies.data.get().map(|rows| rows.items.into_iter().filter(|row| toolkit.with_value(|slug| slug == &row.toolkit)).collect::<Vec<_>>());
                if id.is_empty() || rows.as_ref().is_some_and(Vec::is_empty) { return view! { <Empty title="No saved approvals" /> }.into_any(); }
                let Some(rows) = rows else { return view! { <Loading count=1 /> }.into_any(); };
                view! { <div class="k-card">{rows.into_iter().map(|row| {
                    let path = format!("/api/agents/{}/connected-app-policies", encode(&id));
                    view! { <Row title=row.tool meta=Box::new(move || row.account.into_any()) trailing=Box::new(move || view! {
                        <Button label="Ask next time" size="sm" disabled=move || apps.journal.locked() || apps.policies.busy.get()
                            click=UiAction::new(move |()| apps.execute(Command::new(path.clone(), json!({ "fingerprint": row.fingerprint })))) />
                    }.into_any()) /> }
                }).collect_view()}</div> }.into_any()
            }}
        </Section>
    }
}
