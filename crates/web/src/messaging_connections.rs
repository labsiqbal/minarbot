use crate::{api, connection_ui::*};
use leptos::{prelude::*, task::spawn_local};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};

const DISCORD: &str = "/api/connected-apps/discord";
const WHATSAPP: &str = "/api/whatsapp";

#[derive(Clone, Deserialize, PartialEq)]
struct DiscordStatus {
    generation: String,
    status: String,
    bot: Option<String>,
    routes: Vec<DiscordRoute>,
    deliveries: Vec<Delivery>,
}
#[derive(Clone, Deserialize, PartialEq)]
struct DiscordRoute {
    id: String,
    channel: String,
    kind: String,
    target: String,
    users: Vec<String>,
    label: Option<String>,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Delivery {
    channel: Option<String>,
    status: String,
    part: usize,
    total: usize,
    error: String,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Projects {
    projects: Vec<Project>,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Project {
    name: String,
    archived: bool,
    sections: Vec<Destination>,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Destination {
    id: String,
    name: String,
    archived: bool,
    project: Option<String>,
}

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct WhatsAppStatus {
    state: String,
    connected: bool,
    qr: Option<String>,
    chats: Vec<Chat>,
    delivery_problems: Vec<Problem>,
    transcription: Option<Transcription>,
}
#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Chat {
    chat: String,
    label: String,
    kind: Option<String>,
    target: Option<String>,
    transcribe_provider: Option<String>,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Problem {
    chat: String,
    status: String,
    error: String,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Transcription {
    id: String,
    origin: String,
}

struct Connection<T: Send + Sync + 'static> {
    status: Query<T>,
    busy: RwSignal<bool>,
    error: RwSignal<String>,
    confirmation: Confirm,
}
impl<T: Send + Sync + 'static> Copy for Connection<T> {}
impl<T: Send + Sync + 'static> Clone for Connection<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: DeserializeOwned + PartialEq + Send + Sync + 'static> Connection<T> {
    fn new(path: &'static str) -> Self {
        Self {
            status: query(move || path.into()),
            busy: RwSignal::new(false),
            error: RwSignal::new(String::new()),
            confirmation: Confirm::new(None),
        }
    }
    fn change(self, path: String, body: Value, secret: bool, finish: impl FnOnce() + 'static) {
        if self.busy.get_untracked() {
            return;
        }
        self.busy.set(true);
        self.status.invalidate();
        self.error.set(String::new());
        spawn_local(async move {
            let result = api::command::<T>(&path, body).await;
            if self.busy.try_set(false).is_some() {
                return;
            }
            match result {
                Ok(status) => {
                    self.status.data.set(Some(status));
                    self.status.error.set(String::new());
                    finish();
                }
                Err(_) if secret => self
                    .error
                    .set("Discord could not connect. Check the bot token and try again.".into()),
                Err(message) => self.error.set(message),
            }
        });
    }
    fn poll(self) {
        if !self.busy.get_untracked() && !self.status.busy.get_untracked() {
            self.status.reload.run(());
        }
    }
    fn error(self) -> Signal<String> {
        Signal::derive(move || {
            if self.error.with(String::is_empty) {
                self.status.error.get()
            } else {
                self.error.get()
            }
        })
    }
}

fn discord_change(
    connection: Connection<DiscordStatus>,
    action: &'static str,
    mut body: Value,
    finish: impl FnOnce() + 'static,
) {
    let Some(generation) = connection
        .status
        .data
        .with_untracked(|status| status.as_ref().map(|row| row.generation.clone()))
    else {
        return;
    };
    body["generation"] = json!(generation);
    connection.change(
        format!("{DISCORD}/{action}"),
        body,
        action == "connect",
        finish,
    );
}

pub fn discord() -> impl IntoView {
    let connection = Connection::<DiscordStatus>::new(DISCORD);
    let projects = query::<Projects>(String::new);
    let replace = RwSignal::new(false);
    let add = RwSignal::new(false);
    let destinations = Signal::derive(move || {
        let mut options = agent_options(true)
            .into_iter()
            .map(|(id, name)| (format!("agent:{id}"), name))
            .collect::<Vec<_>>();
        projects.data.with(|projects| {
            if let Some(projects) = projects {
                for project in projects.projects.iter().filter(|project| !project.archived) {
                    options.extend(
                        project
                            .sections
                            .iter()
                            .filter(|section| !section.archived)
                            .map(|section| {
                                (
                                    format!("section:{}", section.id),
                                    format!("{} / {}", project.name, section.name),
                                )
                            }),
                    );
                }
            }
        });
        options
    });
    let can_connect = Memo::new(move |_| {
        connection
            .status
            .data
            .with(|row| row.as_ref().map(|row| row.bot.is_some()))
    });
    let disconnect = UiAction::new(move |()| {
        confirm(
            connection.confirmation,
            "Disconnect Discord?",
            "Discord routes will be removed.",
            "Disconnect",
            true,
            UiAction::new(move |()| {
                discord_change(connection, "disconnect", json!({}), move || {
                    add.set(false);
                    replace.set(false);
                })
            }),
        );
    });
    let status_label = move || {
        connection.status.data.with(|status| {
            status.as_ref().map(|row| match row.status.as_str() {
                "connecting" => "Connecting",
                "reconnecting" => "Reconnecting",
                "connected" => "Connected",
                "invalid_token" => "Token rejected",
                "intents_required" => "Enable Message Content Intent",
                "gateway_rejected" => "Gateway rejected connection",
                "token_unavailable" => "Saved token unavailable",
                _ => "Disconnected",
            })
        })
    };
    poll(2, move || connection.poll());
    view! {
        <Section title="Discord">
            <ErrorNotice error=connection.error() />
            <Show when=move || connection.status.data.with(Option::is_none) && connection.status.error.with(String::is_empty)><Loading count=1 /></Show>
            <Show when=move || can_connect.get().is_some()>
                <div class="k-card"><Row title="Bot connection" meta=Box::new(move || view! {
                    <span role="status">{move || status_label().map(|label| view! { <Badge success=label == "Connected">{label}</Badge> })}</span>
                }.into_any()) trailing=Box::new(move || view! {
                    <Show when=move || can_connect.get() == Some(true)>
                        <div class="k-inline">
                            <Show when=move || !replace.get()>
                                <Button label="Replace token" size="sm" disabled=connection.busy click=UiAction::new(move |()| replace.set(true)) />
                            </Show>
                            <Button label="Disconnect" size="sm" disabled=connection.busy click=disconnect />
                        </div>
                    </Show>
                }.into_any()) /></div>
                <Show when=move || can_connect.get() == Some(false) || replace.get()>
                    <form class="k-fieldset" aria-label="Discord bot connection" on:submit=move |event: web_sys::SubmitEvent| {
                        let Some(fields) = form(&event) else { return };
                        let token = field(&fields, "discordToken");
                        if let Some(form) = event.target().and_then(|node| wasm_bindgen::JsCast::dyn_into::<web_sys::HtmlFormElement>(node).ok()) { form.reset(); }
                        discord_change(connection, "connect", json!({ "token": token }), move || replace.set(false));
                    }>
                        <Field label="Bot token" name="discord-token"><input class="k-input" id="discord-token" name="discordToken"
                            type="password" required maxlength="256" autocomplete="off" spellcheck="false" disabled=connection.busy /></Field>
                        <div class="k-inline">
                            {move || view! { <Button label=if can_connect.get()==Some(true) { "Save token" } else { "Connect Discord" }
                                submit=true variant="primary" busy=connection.busy /> }}
                            <Show when=move || replace.get()>
                                <Button label="Cancel" disabled=connection.busy click=UiAction::new(move |()| replace.set(false)) />
                            </Show>
                        </div>
                    </form>
                </Show>
                <Show when=move || can_connect.get()==Some(true)>
                    <Section title="Discord routes" actions=Box::new(move || view! {
                        <Button label="Add route" size="sm" disabled=move || add.get() || connection.busy.get()
                            click=UiAction::new(move |()| {
                                add.set(true); connection.error.set(String::new());
                                spawn_local(async move {
                                    match api::read::<Projects>("/api/projects").await {
                                        Ok(rows) => { projects.data.try_set(Some(rows)); }
                                        Err(_) => { connection.error.try_set("Project sections could not load.".into()); }
                                    }
                                });
                            }) />
                    }.into_any())>
                        {move || connection.status.data.get().filter(|row| !row.routes.is_empty()).map(|row| view! {
                            <div class="k-card">{row.routes.into_iter().map(|route| {
                                let name = route.label.or_else(|| destinations.with(|options| options.iter()
                                    .find(|(id, _)| id == &format!("{}:{}", route.kind, route.target)).map(|(_, name)| name.clone())))
                                    .unwrap_or_else(|| if route.kind == "section" { "Project section".into() } else { "Agent".into() });
                                let count = route.users.len();
                                let summary = format!("{} · {count} allowed {}", route.channel, if count == 1 { "user" } else { "users" });
                                view! { <Row title=name meta=Box::new(move || summary.into_any()) trailing=Box::new(move || view! {
                                    <Button label="Remove" aria=format!("Remove route {}", route.channel) size="sm" disabled=connection.busy
                                        click=UiAction::new(move |()| discord_change(connection, "remove-route", json!({ "route": route.id }), || {})) />
                                }.into_any()) /> }
                            }).collect_view()}</div>
                        })}
                        <Show when=move || add.get()>{move || discord_route(connection, destinations, add)}</Show>
                    </Section>
                </Show>
                {move || connection.status.data.get().filter(|row| !row.deliveries.is_empty()).map(|row| view! {
                    <Section title="Discord delivery issues"><div class="k-card">
                        {row.deliveries.into_iter().map(|row| {
                            let title = match row.status.as_str() { "uncertain" => "Send unconfirmed", "revoked" => "Send stopped", _ => "Send failed" };
                            let channel = row.channel.map_or_else(String::new, |channel| format!("Channel {channel} · "));
                            let summary = format!("{channel}{} of {} parts confirmed. {}", row.part, row.total, row.error);
                            view! { <Row title=title meta=Box::new(move || summary.into_any()) /> }
                        }).collect_view()}
                    </div></Section>
                })}
            </Show>
            <ConfirmDialog state=connection.confirmation />
        </Section>
    }
}

fn discord_route(
    connection: Connection<DiscordStatus>,
    destinations: Signal<Vec<(String, String)>>,
    add: RwSignal<bool>,
) -> impl IntoView {
    let destination = RwSignal::new(String::new());
    view! {
        <form class="k-fieldset" aria-label="Discord routes" on:submit=move |event: web_sys::SubmitEvent| {
            let Some(fields) = form(&event) else { return };
            let value = destination.get_untracked();
            let Some((kind, target)) = value.split_once(':') else { return };
            let channel = field(&fields, "channel");
            let allowed = field(&fields, "users");
            let users = allowed.split(|ch: char| ch.is_whitespace() || ch == ',').filter(|item| !item.is_empty()).collect::<Vec<_>>();
            discord_change(connection, "add-route", json!({ "channel": channel.trim(), "users": users, "kind": kind, "target": target }), move || add.set(false));
        }>
            <Field label="Discord channel, thread, or DM ID" name="discord-channel">
                <input class="k-input" id="discord-channel" name="channel" required inputmode="numeric" pattern="[1-9][0-9]{0,19}" maxlength="20" disabled=connection.busy />
            </Field>
            <Field label="Allowed Discord user IDs" name="discord-users" hint="Separate IDs with commas.">
                <input class="k-input" id="discord-users" name="users" required maxlength="2200" disabled=connection.busy aria-describedby="discord-users-hint" />
            </Field>
            <Field label="Destination" name="discord-destination">
                <Select name="discord-destination" value=destination.into() disabled=connection.busy
                    options=Signal::derive(move || std::iter::once((String::new(), "Choose agent or section".into())).chain(destinations.get()).collect())
                    change=Callback::new(move |value| destination.set(value)) />
            </Field>
            <div class="k-inline">
                <Button label="Save route" variant="primary" submit=true busy=connection.busy disabled=move || destination.with(String::is_empty) />
                <Button label="Cancel" disabled=connection.busy click=UiAction::new(move |()| add.set(false)) />
            </div>
        </form>
    }
}

fn wa_badge(data: Option<WhatsAppStatus>) -> impl IntoView {
    let success = data
        .as_ref()
        .is_some_and(|data| data.state == "linked" && data.connected);
    let title = match data.as_ref().map(|row| row.state.as_str()) {
        None => "Loading",
        Some("linked") if success => "Linked",
        Some("linked") => "Linked · Reconnecting",
        Some("pairing") => "Waiting for scan",
        Some("needs_relink") => "Needs re-link",
        _ => "Disconnected",
    };
    view! { <Badge success=success>{title}</Badge> }
}

pub fn whatsapp() -> impl IntoView {
    let connection = Connection::<WhatsAppStatus>::new(WHATSAPP);
    let open = RwSignal::new(false);
    let sections = query::<Vec<Destination>>(|| "/api/channels".into());
    let done = UiAction::new(move |()| {
        open.set(false);
    });
    let unlink = UiAction::new(move |()| {
        confirm(
            connection.confirmation,
            "Unlink WhatsApp?",
            "The link session and chat mappings will be deleted.",
            "Unlink",
            true,
            UiAction::new(move |()| {
                connection.change(format!("{WHATSAPP}/unlink"), json!({}), false, || {})
            }),
        )
    });
    poll(15, move || {
        if !open.get_untracked() {
            connection.poll();
        }
    });
    poll(2, move || {
        if open.get_untracked() {
            connection.poll();
        }
    });
    let targets = Signal::derive(move || {
        let mut result = agent_options(true)
            .into_iter()
            .map(|(id, label)| (format!("agents/{id}"), label))
            .collect::<Vec<_>>();
        result.extend(
            sections
                .data
                .get()
                .unwrap_or_default()
                .into_iter()
                .filter(|section| !section.archived && section.project.is_some())
                .map(|row| (format!("channels/{}", row.id), row.name)),
        );
        result
    });
    view! {
        <div class="k-card">
            <Row title="WhatsApp" icon="message" click=UiAction::new(move |()| { open.set(true); connection.poll(); })
                meta=Box::new(move || view! { {move || connection.status.data.get().map(|data| wa_badge(Some(data)))} }.into_any()) />
        </div>
        <Show when=move || open.get()>
            <Modal title="WhatsApp".into() close=done>
                <div class="k-stack">
                    <div class="k-inline" role="status">{move || wa_badge(connection.status.data.get())}</div>
                    <ErrorNotice error=Signal::derive(move || {
                        let message = connection.error.get();
                        if message.is_empty() && !connection.status.error.with(String::is_empty) { "WhatsApp status is unavailable.".into() } else { message }
                    }) />
                    {move || connection.status.data.get().map(|data| match data.state.as_str() {
                        "linked" => {
                            let has_chats = !data.chats.is_empty();
                            let problems = data.delivery_problems;
                            let names = data.chats.iter().map(|chat| (chat.chat.clone(), chat.label.clone())).collect::<std::collections::BTreeMap<_, _>>();
                            let provider = data.transcription;
                            view! {
                                <Section title="Chat mappings">
                                    {if has_chats {
                                        view! { <div class="k-card">{data.chats.into_iter().map(|chat| mapping(connection, chat, provider.clone(), targets)).collect_view()}</div> }.into_any()
                                    } else { view! { <Empty title="No chats seen yet"><div class="k-empty__text">"Receive a WhatsApp message to see its chat here."</div></Empty> }.into_any() }}
                                </Section>
                                {(!problems.is_empty()).then(|| view! { <Section title="Delivery problems"><div class="k-card">
                                    {problems.into_iter().map(|row| {
                                        let name = names.get(&row.chat).cloned().unwrap_or(row.chat);
                                        view! { <Row title=name meta=Box::new(move || row.error.into_any())
                                            trailing=Box::new(move || view! { <span class="k-badge is-warn">{if row.status == "uncertain" { "Unconfirmed" } else { "Failed" }}</span> }.into_any()) /> }
                                    }).collect_view()}
                                </div></Section> })}
                                <div><Button label="Unlink WhatsApp" busy=connection.busy click=unlink /></div>
                            }.into_any()
                        }
                        "pairing" => view! {
                            {data.qr.filter(|qr| qr.starts_with("data:image/svg+xml;base64,") || qr.starts_with("data:image/png;base64,")).map_or_else(
                                || view! { <p class="k-field__hint">"Getting QR code…"</p> }.into_any(),
                                |qr| view! { <div class="wa-settings__pair">
                                    <img src=qr width="258" height="258" alt="WhatsApp linking QR code" />
                                    <p class="k-field__hint">"On your phone, open WhatsApp → Linked devices → Link a device."</p>
                                </div> }.into_any()
                            )}
                            <div><Button label="Cancel linking" busy=connection.busy
                                click=UiAction::new(move |()| connection.change(format!("{WHATSAPP}/unlink"), json!({}), false, || {})) /></div>
                        }.into_any(),
                        _ => {
                            let relink = data.state == "needs_relink";
                            view! {
                                <Alert tone="warn">"Unofficial WhatsApp Web link. Protocol updates may break it, and WhatsApp can ban your number."</Alert>
                                <div class="k-inline">
                                    <Button label=if relink { "Link again" } else { "Link WhatsApp" } variant="primary" busy=connection.busy
                                        click=UiAction::new(move |()| connection.change(format!("{WHATSAPP}/link"), json!({}), false, || {})) />
                                    {relink.then(|| view! { <Button label="Unlink WhatsApp" disabled=connection.busy click=unlink /> })}
                                </div>
                            }.into_any()
                        }
                    })}
                </div>
            </Modal>
        </Show>
        <ConfirmDialog state=connection.confirmation />
    }
}

fn mapping(
    connection: Connection<WhatsAppStatus>,
    chat: Chat,
    provider: Option<Transcription>,
    targets: Signal<Vec<(String, String)>>,
) -> impl IntoView {
    let initial = StoredValue::new(
        chat.kind
            .zip(chat.target)
            .map_or_else(String::new, |(kind, target)| format!("{kind}/{target}")),
    );
    let initial_voice = provider
        .as_ref()
        .is_some_and(|provider| chat.transcribe_provider.as_deref() == Some(&provider.id));
    let target = RwSignal::new(initial.get_value());
    let voice = RwSignal::new(initial_voice);
    let dirty = move || {
        target.with(|target| {
            initial.with_value(|initial| initial != target)
                || !target.is_empty() && voice.get() != initial_voice
        })
    };
    let description = provider.map(|provider| format!("Send audio to {}", provider.origin));
    view! {
        <form class="wa-settings__mapping" on:submit=move |event: web_sys::SubmitEvent| {
            event.prevent_default();
            if !dirty() { return; }
            let destination = target.get_untracked();
            let (kind, target) = destination.split_once('/').unzip();
            connection.change(format!("{WHATSAPP}/map"), json!({ "chat": chat.chat, "kind": kind, "target": target,
                "transcribe": !destination.is_empty() && voice.get_untracked() }), false, || {});
        }>
            <label class="k-field">
                <span class="k-field__label">{chat.label.clone()}</span>
                <Select name="whatsapp-destination" label=format!("Destination for {}", chat.label) value=target.into() disabled=connection.busy
                    options=Signal::derive(move || {
                        let mut items = targets.get();
                        let current = initial.get_value();
                        if !current.is_empty() && !items.iter().any(|(id, _)| id == &current) { items.push((current, "Destination unavailable".into())); }
                        items.insert(0, (String::new(), "Unmapped".into()));
                        items
                    }) change=Callback::new(move |value| target.set(value)) />
            </label>
            {description.map(|description| view! { <Show when=move || !target.with(String::is_empty)>
                <Check label="Transcribe voice notes" checked=voice disabled=connection.busy change=Callback::new(move |value| voice.set(value)) />
                <span class="k-field__hint">{description.clone()}</span>
            </Show> })}
            <Button label="Save mapping" size="sm" submit=true disabled=move || !dirty() || connection.busy.get() />
        </form>
    }
}
