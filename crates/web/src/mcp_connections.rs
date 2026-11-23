use crate::{
    api,
    connection_ui::*,
    settings_recovery::{Command, Journal, Recovery, UUID},
};
use leptos::{prelude::*, task::spawn_local};
use serde::Deserialize;
use serde_json::{json, Value};

const PATH: &str = "/api/mcp";

#[derive(Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Connection {
    id: String,
    name: String,
    url: String,
    has_token: bool,
    enabled: u8,
    revision: u64,
}
#[derive(Clone, PartialEq, Deserialize)]
pub(crate) struct Page {
    items: Vec<Connection>,
    next_after: Option<String>,
}
#[derive(Clone, PartialEq, Deserialize)]
struct Tool {
    name: String,
    title: Option<String>,
    description: Option<String>,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
    #[serde(default, rename = "_meta")]
    meta: Option<serde_json::Map<String, Value>>,
}
#[derive(Clone, PartialEq, Deserialize)]
struct Detail {
    #[serde(flatten)]
    connection: Connection,
    tools: Vec<Tool>,
    grant: Grant,
}
#[derive(Clone, PartialEq, Deserialize)]
struct Grant {
    revision: u64,
    tools: Vec<String>,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Test {
    id: String,
    tools: Vec<Tool>,
    expires: f64,
}

impl Tool {
    fn meta(&self, field: &str) -> Option<&str> {
        self.meta.as_ref()?.get(field)?.as_str()
    }
    fn label(&self) -> String {
        let title = self.title.as_deref().unwrap_or(&self.name);
        match self.meta("minarbot.kind") {
            Some("resource-template") => format!("Resource template: {title}"),
            Some("resource") => format!("Resource: {title}"),
            Some("prompt") => format!("Prompt: {title}"),
            _ if title != self.name => format!("Tool: {title} ({})", self.name),
            _ => format!("Tool: {title}"),
        }
    }
}

#[derive(Clone, Copy)]
struct State {
    page: Query<Page>,
    first_page: Option<StoredValue<Option<Page>>>,
    agent: RwSignal<String>,
    agents: Signal<Vec<(String, String)>>,
    selected: RwSignal<String>,
    detail: Query<Detail>,
    tools: RwSignal<Vec<String>>,
    editing: RwSignal<Option<Connection>>,
    draft: RwSignal<bool>,
    open: RwSignal<bool>,
    name: RwSignal<String>,
    url: RwSignal<String>,
    token: RwSignal<String>,
    rotate: RwSignal<bool>,
    tested: RwSignal<Option<Test>>,
    testing: RwSignal<bool>,
    confirmation: Confirm,
    journal: Journal,
    error: RwSignal<String>,
}

impl State {
    fn refresh(self) {
        self.page.reload.run(());
        self.detail.reload.run(());
    }
    fn locked(self) -> bool {
        self.testing.get() || self.journal.locked()
    }
    fn reset(self, connection: Option<Connection>) {
        self.name.set(
            connection
                .as_ref()
                .map_or_else(String::new, |row| row.name.clone()),
        );
        self.url.set(
            connection
                .as_ref()
                .map_or_else(String::new, |row| row.url.clone()),
        );
        self.editing.set(connection);
        self.token.set(String::new());
        self.rotate.set(false);
        self.tested.set(None);
        self.error.set(String::new());
        self.draft.set(true);
    }
    fn edit(self, row: Connection) {
        let apply = UiAction::new(move |()| {
            self.reset(Some(row.clone()));
            self.open.set(true);
        });
        if self.draft.get_untracked()
            && (!self.name.with_untracked(String::is_empty)
                || !self.url.with_untracked(String::is_empty)
                || !self.token.with_untracked(String::is_empty))
        {
            confirm(
                self.confirmation,
                "Discard the current connection draft and open this connection?",
                "",
                "Discard draft",
                true,
                apply,
            );
        } else {
            apply.run(());
        }
    }
    fn execute(self, command: Command) {
        self.error.set(String::new());
        let test = command.body.get("test").is_some();
        let remove = command.path.ends_with("/remove");
        if let Some(cache) = self.first_page {
            cache.set_value(None);
        }
        self.journal.run(command, None, move |result| {
            if result.is_none() {
                return;
            }
            if test {
                self.open.set(false);
                self.reset(None);
                self.draft.set(false);
            }
            if remove {
                self.selected.set(String::new());
            }
            self.refresh();
            crate::settings::focus_heading();
        });
    }
    fn test(self) {
        if self.locked() {
            return;
        }
        let secret = self.token.get_untracked();
        let mut body = json!({ "id": uuid(), "name": self.name.get_untracked() });
        if let Some(row) = self.editing.get_untracked() {
            body["connection"] = json!(row.id);
            body["revision"] = json!(row.revision);
            if self.rotate.get_untracked() {
                body["token"] = json!(secret);
            }
        } else {
            body["url"] = json!(self.url.get_untracked());
            body["token"] = json!(secret);
        }
        self.tested.set(None);
        self.testing.set(true);
        self.error.set(String::new());
        let path = self.journal.endpoint("/api/mcp/test");
        spawn_local(async move {
            match api::post::<Test>(&path, &body).await {
                Ok(result) => {
                    self.tested.try_set(Some(result));
                }
                Err(message) => {
                    self.error.try_set(redact(message, &secret));
                }
            }
            self.testing.try_set(false);
        });
    }
    fn reload_revision(self) {
        let Some(row) = self.editing.get_untracked() else {
            return;
        };
        confirm(self.confirmation, "Load the latest connection revision for this draft?",
            "The entered name and token stay saved, but old test results are discarded. Test again before saving.",
            "Reload", false, UiAction::new(move |()| {
                let path = self.journal.endpoint(&format!("{PATH}/{}", encode(&row.id)));
                self.testing.set(true);
                self.tested.set(None);
                self.error.set(String::new());
                spawn_local(async move {
                    match api::read::<Connection>(&path).await {
                        Ok(row) => { self.editing.try_set(Some(row)); if self.page.busy.try_get_untracked().is_some() { self.page.reload.run(()); } }
                        Err(message) => { self.error.try_set(message); }
                    }
                    self.testing.try_set(false);
                });
            }));
    }
}

pub fn panel() -> impl IntoView {
    scoped_panel(None, None, None)
}

pub(crate) fn scoped_panel(
    project: Option<String>,
    members: Option<Vec<String>>,
    first_page: Option<StoredValue<Option<Page>>>,
) -> impl IntoView {
    let agents = scoped_agents(false, members.map(StoredValue::new));
    let journal = Journal::new(
        project.as_ref().map_or_else(
            || "minarbot:mcp-command".into(),
            |id| format!("minarbot:mcp-command:{id}"),
        ),
        16000,
        |cmd| {
            cmd.valid(
                &format!("^/api/(mcp(/{UUID}(/remove)?)?|agents/{UUID}/mcp/{UUID})$"),
                &[
                    "test",
                    "revision",
                    "enabled",
                    "connectionRevision",
                    "tools",
                    "confirmed",
                ],
            ) && cmd.body.get("tools").is_none_or(|tools| {
                tools
                    .as_array()
                    .is_some_and(|items| items.iter().all(serde_json::Value::is_string))
            })
        },
    )
    .for_project(project.map(StoredValue::new));
    let cursor = Cursor::new(Vec::new());
    let selected = RwSignal::new(String::new());
    let agent = selected_agent(agents);
    let state = State {
        page: cached_query(
            move || journal.endpoint(&format!("{PATH}{}", cursor_query(cursor))),
            first_page,
        ),
        first_page,
        agent,
        agents,
        selected,
        detail: query(move || {
            let selected = selected.get_untracked();
            let agent = agent.get_untracked();
            if selected.is_empty() || agent.is_empty() {
                String::new()
            } else {
                journal.endpoint(&format!(
                    "{PATH}/{}?agent={}",
                    encode(&selected),
                    encode(&agent)
                ))
            }
        }),
        tools: RwSignal::new(Vec::new()),
        editing: RwSignal::new(None),
        draft: RwSignal::new(false),
        open: RwSignal::new(false),
        name: RwSignal::new(String::new()),
        url: RwSignal::new(String::new()),
        token: RwSignal::new(String::new()),
        rotate: RwSignal::new(false),
        tested: RwSignal::new(None),
        testing: RwSignal::new(false),
        confirmation: Confirm::new(None),
        error: RwSignal::new(String::new()),
        journal,
    };
    Effect::new(move |_| {
        let _ = selected.get();
        let _ = agent.get();
        state.detail.data.set(None);
        state.detail.reload.run(());
    });
    Effect::new(move |_| {
        if let Some(detail) = state.detail.data.get() {
            state.tools.set(detail.grant.tools);
        }
    });
    Effect::new(move |_| {
        let _ = selected.get();
        let _ = cursor.get();
        request_animation_frame(move || {
            if let Ok(Some(heading)) = document().query_selector(".bs-set3-mcp h1") {
                if let Some(heading) =
                    wasm_bindgen::JsCast::dyn_ref::<web_sys::HtmlElement>(&heading)
                {
                    let _ = heading.focus();
                }
            }
        });
    });
    let time = RwSignal::new(js_sys::Date::now());
    poll(1, move || {
        if state.tested.with_untracked(Option::is_some) {
            time.set(js_sys::Date::now());
            if state.tested.with_untracked(|test| {
                test.as_ref()
                    .is_some_and(|test| test.expires <= time.get_untracked())
            }) {
                state.tested.set(None);
            }
        }
    });
    poll(5, move || {
        if !state.locked()
            && !state.open.get_untracked()
            && state.detail.data.with_untracked(|detail| {
                detail.as_ref().is_none_or(|row| {
                    state
                        .tools
                        .with_untracked(|tools| tools == &row.grant.tools)
                })
            })
        {
            state.refresh();
        }
    });
    let reload = UiAction::new(move |()| state.refresh());
    let retry = UiAction::new(move |()| {
        if let Some(command) = state.journal.pending.get_untracked() {
            state.execute(command);
        }
    });
    let connection = Memo::new(move |_| {
        state
            .detail
            .data
            .with(|data| data.as_ref().map(|row| row.connection.clone()))
            .or_else(|| {
                state.page.data.with(|data| {
                    data.as_ref()?
                        .items
                        .iter()
                        .find(|row| selected.with(|selected| selected == &row.id))
                        .cloned()
                })
            })
    });
    let error = Signal::derive(move || {
        [
            state.error.get(),
            state.journal.error.get(),
            state.page.error.get(),
            state.detail.error.get(),
        ]
        .into_iter()
        .find(|value| !value.is_empty())
        .unwrap_or_default()
    });
    view! {
        <div class="k-page bs-root bs-set3-mcp">
            <Show when=move || !selected.with(String::is_empty)>
                <Button label="MCP connections" aria="Back to MCP connections" variant="ghost" icon="arrow-left"
                    click=UiAction::new(move |()| selected.set(String::new())) />
            </Show>
            <Header title=move || if selected.with(String::is_empty) { "MCP connections".into() }
                else { connection.get().map_or_else(|| "Connection".into(), |row| row.name) }
                actions=Box::new(move || view! {
                    {move || match connection.get() {
                        Some(row) => view! {
                            <Show when=move || state.editing.with(Option::is_some)><Button label="Continue editing" disabled=move || state.locked()
                                click=UiAction::new(move |()| state.open.set(true)) /></Show>
                            {menu(state, row)}
                        }.into_any(),
                        None => view! { {move || view! {
                            <Button label=if state.editing.with(Option::is_some) { "Continue editing" } else { "Add connection" }
                                variant="primary" icon="plus" disabled=move || state.locked() click=UiAction::new(move |()| state.open.set(true)) />
                        }} }.into_any(),
                    }}
                }.into_any()) />
            <ErrorNotice error=error retry=reload />
            <Recovery journal=state.journal retry=retry confirmation=state.confirmation refresh=reload />
            <Show when=move || selected.with(String::is_empty) fallback=move || permissions(state, connection)>
                <div id="mcp-connections" class="k-stack">
                    <Show when=move || state.page.data.with(Option::is_none) && state.page.error.with(String::is_empty)>
                        <Loading label="Loading connections..." />
                    </Show>
                    <Show when=move || state.page.data.with(|data| data.as_ref().is_some_and(|page| page.items.is_empty()))>
                        <Empty title="No connections" icon="plug" />
                    </Show>
                    <Show when=move || state.page.data.with(|data| data.as_ref().is_some_and(|page| !page.items.is_empty()))>
                        <div class="k-card">
                            <For each=move || state.page.data.get().map_or_else(Vec::new, |data| data.items)
                                key=|row| (row.id.clone(), row.revision, row.enabled, row.name.clone())
                                children=move |row| {
                                    let id = StoredValue::new(row.id.clone());
                                    let address = host(&row.url);
                                    let enabled = row.enabled != 0;
                                    view! { <Row title=row.name.clone() icon="plug" disabled=move || state.locked()
                                        click=UiAction::new(move |()| selected.set(id.get_value()))
                                        meta=Box::new(move || view! { <span>{address}</span>{status(enabled)} }.into_any())
                                        trailing=Box::new(move || menu(state, row).into_any()) /> }
                                } />
                        </div>
                    </Show>
                    <Pager cursor=cursor next=Signal::derive(move || state.page.data.get().and_then(|page| page.next_after))
                        previous_label="Previous connections" next_label="Next connections" refresh=reload disabled=move || state.locked() />
                </div>
            </Show>
            <Show when=move || state.open.get()>{move || editor(state, error, reload, retry, time)}</Show>
            <ConfirmDialog state=state.confirmation />
        </div>
    }
}

fn status(enabled: bool) -> impl IntoView {
    view! { <Badge success=enabled>{if enabled { "Active" } else { "Inactive" }}</Badge> }
}

fn menu(state: State, row: Connection) -> impl IntoView {
    let path = format!("{PATH}/{}", encode(&row.id));
    let row = StoredValue::new(row);
    let name = row.with_value(|row| row.name.clone());
    let enabled = row.with_value(|row| row.enabled != 0);
    let revision = row.with_value(|row| row.revision);
    let toggle = {
        let path = path.clone();
        UiAction::new(move |()| {
            state.execute(Command::new(
                path.clone(),
                json!({ "revision": revision, "enabled": !enabled }),
            ))
        })
    };
    let remove = UiAction::new(move |()| {
        let path = format!("{path}/remove");
        confirm(state.confirmation, row.with_value(|row| format!("Remove {}?", row.name)),
            "This removes the saved connection and revokes agent access. External effects already completed remain.",
            "Remove connection", true, UiAction::new(move |()| state.execute(Command::new(path.clone(), json!({ "confirmed": true })))));
    });
    view! { <Menu label=format!("Menu {name}") disabled=move || state.locked() || state.page.busy.get()
    items=vec![
        MenuItem::new("Edit connection", UiAction::new(move |()| state.edit(row.get_value()))),
        MenuItem::new(format!("{} {name}", if enabled { "Disable" } else { "Enable" }), toggle),
        MenuItem::new(format!("Remove {name}"), remove).danger(),
    ] /> }
}

fn permissions(state: State, connection: Memo<Option<Connection>>) -> impl IntoView {
    view! {
        {move || connection.get().map(|row| {
            let enabled = row.enabled != 0;
            view! { <div class="k-card">
                <Row title="Status" trailing=Box::new(move || status(enabled).into_any()) />
                <Row title="URL" meta=Box::new(move || row.url.into_any()) />
                <Row title="Token" trailing=Box::new(move || view! { <Badge>{if row.has_token { "Saved" } else { "None" }}</Badge> }.into_any()) />
            </div> }
        })}
        <Section title="Permissions" actions=Box::new(move || view! {
            <Select name="mcp-agent" label="Agent" small=true value=state.agent.into()
                options=Signal::derive(move || state.agents.get()) disabled=move || state.locked()
                change=Callback::new(move |value| state.agent.set(value)) />
        }.into_any())>
            {move || {
                if state.agent.with(String::is_empty) { return view! { <Empty title="No agents yet" icon="plug" /> }.into_any(); }
                let Some(detail) = state.detail.data.get() else {
                    return if state.detail.error.with(String::is_empty) { view! { <Loading label="Loading permissions..." /> }.into_any() } else { ().into_any() };
                };
                let id = StoredValue::new(detail.connection.id);
                let revision = detail.grant.revision;
                let connection_revision = detail.connection.revision;
                view! {
                    <form class="k-stack" on:submit=move |event: web_sys::SubmitEvent| {
                        event.prevent_default();
                        if state.locked() || state.detail.busy.get_untracked() { return; }
                        let current = state.agent.get_untracked();
                        let name = state.agents.get().into_iter().find(|(id, _)| id == &current).map_or_else(|| "agent".into(), |(_, name)| name);
                        confirm(state.confirmation, format!("Allow selected access for {name}?"),
                            "Effects in external services may still occur after the task is canceled.", "Save permissions", false,
                            UiAction::new(move |()| state.execute(Command::new(
                                format!("/api/agents/{}/mcp/{}", encode(&current), encode(&id.get_value())),
                                json!({ "revision": revision, "connectionRevision": connection_revision, "tools": state.tools.get_untracked() })))));
                    }>
                        <fieldset class="k-fieldset" disabled=move || state.locked()>
                            {if detail.tools.is_empty() {
                                view! { <Empty title="No tools, resources or prompts" /> }.into_any()
                            } else {
                                view! { <div class="k-card">{detail.tools.into_iter().map(|tool| {
                                    let name = StoredValue::new(tool.name.clone());
                                    let label = tool.label();
                                    let warning = tool.meta("minarbot.kind") == Some("resource-template");
                                    let target = tool.meta("minarbot.target").map(str::to_owned);
                                    view! { <div>
                                        <div class="k-row"><div class="k-row__main">
                                            <Check label=label.clone() checked=move || state.tools.with(|tools| name.with_value(|name| tools.contains(name)))
                                                disabled=move || state.locked() change=Callback::new(move |checked| state.tools.update(|tools| {
                                                    if checked { tools.push(name.get_value()); } else { tools.retain(|item| name.with_value(|name| item != name)); }
                                                })) />
                                        </div></div>
                                        <div class="mcp-tool-details"><Disclosure title=format!("Details: {}", tool.title.as_deref().unwrap_or(&tool.name))>
                                            {tool.description.map(|description| view! { <p class="k-field__hint">{description}</p> })}
                                            {warning.then(|| view! { <Alert tone="warn">"Permissions cover all URIs this template can form, including parameter values chosen by the agent. The target server still controls access to its data."</Alert> })}
                                            {target.map(|target| view! { <span class="k-field__hint">{target}</span> })}
                                            <Code label=format!("Parameters for {label}") text=serde_json::to_string_pretty(&tool.input_schema).unwrap_or_default() />
                                        </Disclosure></div>
                                    </div> }
                                }).collect_view()}</div> }.into_any()
                            }}
                        </fieldset>
                        <div class="k-row-actions"><Button label="Save permissions" variant="primary" submit=true disabled=move || state.locked() || state.detail.busy.get() /></div>
                    </form>
                }.into_any()
            }}
        </Section>
    }
}

fn editor(
    state: State,
    error: Signal<String>,
    reload: UiAction,
    retry: UiAction,
    time: RwSignal<f64>,
) -> impl IntoView {
    let row = state.editing.get_untracked();
    let edit = row.is_some();
    let title = row.map_or_else(
        || "New connection".into(),
        |row| format!("Edit {}", row.name),
    );
    let close = UiAction::new(move |()| state.open.set(false));
    let save = UiAction::new(move |()| {
        let Some(test) = state
            .tested
            .get_untracked()
            .filter(|test| test.expires > js_sys::Date::now())
        else {
            return;
        };
        if state.locked() {
            return;
        }
        if let Some(row) = state.editing.get_untracked() {
            confirm(state.confirmation, "Save the tested catalog and token, revoke all access permissions, and stop active MCP calls for this connection?",
                "External effects that have already occurred cannot be undone.", "Save changes", true, UiAction::new(move |()| {
                    if test.expires > js_sys::Date::now() {
                        state.execute(Command::new(format!("{PATH}/{}", encode(&row.id)), json!({ "test": test.id, "revision": row.revision })));
                    }
                }));
        } else {
            state.execute(Command::new(PATH, json!({ "test": test.id })));
        }
    });
    view! {
        <Modal title=title size="lg" close=close busy=move || state.testing.get() || state.journal.busy.get()
            footer=Box::new(move || view! {
                {edit.then(|| view! {
                    <Button label="Load current revision" variant="ghost" disabled=move || state.locked() click=UiAction::new(move |()| state.reload_revision()) />
                    <span class="k-dialog__foot-start"><Button label="Cancel editing" disabled=move || state.locked()
                        click=UiAction::new(move |()| { state.reset(None); close.run(()); }) /></span>
                })}
                <Button label="Test connection" variant="primary" submit=true form="mcp-connection" busy=state.testing disabled=move || state.locked() />
                <Show when=move || state.tested.with(Option::is_some)>
                    <Button label=if edit { "Save changes" } else { "Save connection" } variant="primary" click=save
                        disabled=move || state.locked() || state.tested.with(|test| test.as_ref().is_none_or(|test| test.expires <= time.get())) />
                </Show>
            }.into_any())>
            <ErrorNotice error=error />
            <Recovery journal=state.journal retry=retry confirmation=state.confirmation refresh=reload />
            <form id="mcp-connection" on:submit=move |event: web_sys::SubmitEvent| { event.prevent_default(); state.test(); }>
                <fieldset class="k-fieldset" disabled=move || state.locked()>
                    <Field label="Name" name="mcp-name"><input class="k-input" id="mcp-name" name="name" required maxlength="80" prop:value=state.name
                        on:input=move |event| { state.name.set(event_target_value(&event)); state.tested.set(None); } /></Field>
                    <Field label="URL" name="mcp-url"><input class="k-input" id="mcp-url" name="url" type="url" required readonly=edit maxlength="2000" prop:value=state.url
                        on:input=move |event| { state.url.set(event_target_value(&event)); state.tested.set(None); } /></Field>
                    {edit.then(|| view! { <Check label="Replace token" checked=state.rotate change=Callback::new(move |value| { state.rotate.set(value); state.token.set(String::new()); state.tested.set(None); }) /> })}
                    <Show when=move || !edit || state.rotate.get()>
                        <Field label="Token" name="mcp-token" optional=true>
                            <input class="k-input" id="mcp-token" name="token" type="password" autocomplete="off" maxlength="4096" prop:value=state.token
                                on:input=move |event| { state.token.set(event_target_value(&event)); state.tested.set(None); } />
                        </Field>
                    </Show>
                    <button type="button" class="k-icon-btn" aria-label="Connection details"
                        title="Streamable HTTP. Testing contacts the service. Saved URLs cannot be changed. Token uses Bearer authentication; an empty replacement removes authentication.">
                        <Icon name="info" />
                    </button>
                    {move || state.tested.get().map(|test| view! {
                        <div class="k-inline"><span tabindex="0" title="Valid for 10 minutes"><Badge success=true>"Tested"</Badge></span>
                            <span class="k-field__hint">{format!("{} items", test.tools.len())}</span></div>
                        <div class="k-card">{test.tools.into_iter().map(|tool| view! { <Row title=tool.label() /> }).collect_view()}</div>
                    })}
                </fieldset>
            </form>
        </Modal>
    }
}
