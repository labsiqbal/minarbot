use super::*;
use crate::{api, connection_ui as ui, shell::machine_status, system::kit};
use serde::Serialize;
use serde_json::Value;

const MESSAGES: &str = "/api/remote-agents/messages";

#[derive(Clone, Deserialize, PartialEq)]
struct PeerAgent {
    id: String,
    name: String,
    model: Option<String>,
}

#[derive(Clone, Deserialize, PartialEq)]
struct Binding {
    provider_account: Option<String>,
    runnable: bool,
    blocked_reason: Option<String>,
}

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Account {
    id: String,
    name: String,
    default_model: Option<String>,
}

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Machine {
    id: String,
    name: String,
    origin: String,
    status: String,
    agents: Vec<PeerAgent>,
    remote_agent_messaging: Option<bool>,
    provider_accounts: Option<Vec<Account>>,
    agent_providers: Option<HashMap<String, Binding>>,
}

#[derive(Clone, Deserialize, PartialEq)]
struct Directory {
    machines: Vec<Machine>,
}

#[derive(Clone, Deserialize, PartialEq)]
struct Message {
    id: String,
    text: String,
    status: String,
    state: String,
    reply: Option<String>,
    error: Option<String>,
    created: i64,
}

#[derive(Clone, Deserialize, PartialEq)]
struct History {
    messages: Vec<Message>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Command {
    id: String,
    machine: String,
    agent: String,
    text: String,
}

fn restore(key: &str, machine: &str, agent: &str) -> Result<Option<Command>, ()> {
    let Some(raw) = session().ok_or(())?.get_item(key).map_err(|_| ())? else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(None);
    }
    if raw.encode_utf16().count() > 100_000 {
        return Err(());
    }
    let command: Command = serde_json::from_str(&raw).map_err(|_| ())?;
    let uuid_v4 = |id: &str| {
        protocol::id(id)
            && id.as_bytes()[14] == b'4'
            && matches!(
                id.as_bytes()[19].to_ascii_lowercase(),
                b'8' | b'9' | b'a' | b'b'
            )
    };
    if ![&command.id, &command.machine, &command.agent]
        .into_iter()
        .all(|id| uuid_v4(id))
        || command.machine != machine
        || command.agent != agent
        || command.text.trim().is_empty()
        || command.text.encode_utf16().count() > 16_000
    {
        return Err(());
    }
    Ok(Some(command))
}

fn binding<'a>(machine: &'a Machine, agent: &PeerAgent) -> Option<&'a Binding> {
    machine.agent_providers.as_ref()?.get(&agent.id)
}

fn disabled_reason(machine: Option<&Machine>, agent: Option<&PeerAgent>) -> Option<String> {
    let Some(machine) = machine else {
        return Some("Remote machine unavailable. Reconnect the machine before sending.".into());
    };
    match machine.status.as_str() {
        "stale" => Some("Waiting for a fresh machine status check.".into()),
        "online" if machine.remote_agent_messaging == Some(false) => Some(format!(
            "Messaging is turned off on {}. Enable it in Settings > Machines on that machine.",
            machine.name
        )),
        "online" => agent
            .and_then(|agent| binding(machine, agent))
            .filter(|binding| !binding.runnable)
            .and_then(|binding| binding.blocked_reason.clone()),
        status => Some(format!(
            "{}. Reconnect the machine before sending.",
            machine_status(status)
        )),
    }
}

fn turn(item: Message, name: &str) -> impl IntoView {
    let date = date_at(item.created);
    let time = date_format(
        &date,
        &js_sys::Array::of1(&"en-US".into()),
        &[("hour", "2-digit"), ("minute", "2-digit")],
    );
    let uncertain = item.state == "uncertain" || item.status == "interrupted";
    let (status, tone) = if uncertain {
        ("Uncertain", "error")
    } else {
        match item.status.as_str() {
            "done" => ("Done", "done"),
            "running" => ("Replying", "working"),
            "failed" => ("Failed", "error"),
            "cancelled" => ("Cancelled", "done"),
            "waiting" => ("Waiting", "attention"),
            _ => ("Queued", "working"),
        }
    };
    let activity = match item.status.as_str() {
        _ if uncertain => "Interrupted",
        "done" => "Worked",
        "running" => "Working",
        "failed" => "Failed",
        "cancelled" => "Cancelled",
        "waiting" => "Waiting",
        _ => "Queued",
    };
    let waiting = item.reply.as_deref().is_none_or(str::is_empty)
        && !matches!(item.status.as_str(), "failed" | "interrupted" | "cancelled");
    let waiting_text = format!("Waiting for {name}...");
    view! {
        <article class="turn-block" id=format!("turn-{}", item.id) tabindex="-1">
            <div class="human-message" tabindex="-1" aria-label="Your message">
                {(!item.text.trim().is_empty()).then(|| view! { <div class="human-bubble message-plain">{item.text}</div> })}
                <div class="turn-meta human-meta"><time class="message-time" datetime=date.to_iso_string().as_string()>{time}</time></div>
            </div>
            <div class="agent-message"><div class="message-column">
                <div class="turn-activity"><div class="turn-activity__header"><span class="activity-label">
                    {(item.status == "running").then(|| view! { <span class="activity-working" aria-hidden="true"></span> })}
                    {activity}
                </span></div></div>
                <div class="message-body">
                    {item.error.filter(|error| !error.is_empty()).map(|error| view! { <p class="message-error" role="alert">{error}</p> })}
                    {item.reply.filter(|reply| !reply.is_empty()).map(|reply| view! { <div class="legacy">{body(reply, String::new())}</div> })}
                    {waiting.then(|| view! { <p class="message-muted" role="status">{waiting_text}</p> })}
                </div>
                <div class="turn-meta agent-meta"><span class=format!("turn-status is-{tone}")><i aria-hidden="true"></i>{status}</span></div>
            </div></div>
        </article>
    }
}

pub(super) fn page(machine: String, agent: String) -> impl IntoView {
    let key = format!("minarbot:remote-message:{machine}:{agent}");
    let recovery = restore(&key, &machine, &agent);
    let invalid = RwSignal::new(recovery.is_err());
    let pending = RwSignal::new(recovery.ok().flatten());
    let draft = format!("{key}:draft");
    let text = RwSignal::new(pending.with_untracked(|command| {
        command
            .as_ref()
            .map(|command| command.text.clone())
            .or_else(|| saved(&draft))
            .unwrap_or_default()
    }));
    let key = StoredValue::new(key);
    let draft = StoredValue::new(draft);
    let machine_id = StoredValue::new(machine);
    let agent_id = StoredValue::new(agent);
    let directory = ui::query::<Directory>(|| "/api/machines".into());
    poll_visible(directory.reload, 5000);
    let machine = Memo::new(move |_| {
        directory.data.with(|data| {
            data.as_ref()?
                .machines
                .iter()
                .find(|machine| machine_id.with_value(|id| *id == machine.id))
                .cloned()
        })
    });
    let agent = Memo::new(move |_| {
        machine.with(|machine| {
            machine
                .as_ref()?
                .agents
                .iter()
                .find(|agent| agent_id.with_value(|id| *id == agent.id))
                .cloned()
        })
    });
    let history = ui::query::<History>(move || {
        if agent.with_untracked(Option::is_none) {
            return String::new();
        }
        format!(
            "{MESSAGES}?machine={}&agent={}",
            machine_id.get_value(),
            agent_id.get_value()
        )
    });
    Effect::new(move |_| {
        agent.with(Option::is_some);
        history.reload.run(());
    });
    let busy = RwSignal::new(false);
    poll_visible(
        Callback::new(move |()| {
            if !busy.get_untracked() && agent.with_untracked(Option::is_some) {
                history.reload.run(());
            }
        }),
        1000,
    );
    let reason = Memo::new(move |_| {
        if invalid.get() {
            Some(
                "Saved message could not be read. Discard it after checking conversation history."
                    .into(),
            )
        } else {
            machine.with(|machine| {
                agent.with(|agent| disabled_reason(machine.as_ref(), agent.as_ref()))
            })
        }
    });
    let error = RwSignal::new(String::new());
    let send = move || {
        let value = text.get_untracked();
        if busy.get_untracked()
            || reason.with_untracked(Option::is_some)
            || agent.with_untracked(Option::is_none)
            || value.trim().is_empty()
            || value.encode_utf16().count() > 16_000
        {
            return;
        }
        let command = match pending.get_untracked() {
            Some(command) => command,
            None => {
                let Some(id) = uuid() else {
                    error.set("Message ID could not be created.".into());
                    return;
                };
                Command {
                    id,
                    machine: machine_id.get_value(),
                    agent: agent_id.get_value(),
                    text: value.trim().into(),
                }
            }
        };
        let key = key.get_value();
        let draft = draft.get_value();
        let Ok(raw) = serde_json::to_string(&command) else {
            return;
        };
        if !save(&key, &raw) {
            error.set("Message could not be saved in browser storage.".into());
            return;
        }
        pending.set(Some(command.clone()));
        busy.set(true);
        history.invalidate();
        error.set(String::new());
        leptos::task::spawn_local(async move {
            let result = api::post::<Value>(MESSAGES, &command).await;
            let clear =
                result.is_ok() && saved(&key).as_deref() == Some(raw.as_str()) && remove(&key);
            if clear {
                remove(&draft);
            }
            if busy.try_get_untracked().is_none() {
                return;
            }
            busy.set(false);
            match result {
                Ok(_) if clear => {
                    pending.set(None);
                    text.set(String::new());
                    history.reload.run(());
                }
                Ok(_) => {
                    error.set("Message was accepted, but recovery could not be cleared.".into())
                }
                Err(message) => error.set(message),
            }
        });
    };
    let confirm = use_context::<kit::Confirm>();
    let discard = move || {
        kit::ask(confirm, "Discard saved message?",
        "Delivery may already have succeeded. Check this conversation and the receiving machine before sending a replacement.",
        "Discard message", true, move || {
            if key.with_value(|key| remove(key)) { pending.set(None); invalid.set(false); }
            else { error.set("Browser storage could not be updated.".into()); }
        }
    )
    };
    let label = move || {
        agent.with(|agent| {
            machine.with(|machine| {
                agent.as_ref().zip(machine.as_ref()).map_or_else(
                    || "Message remote agent".into(),
                    |(agent, machine)| format!("Message {} on {}", agent.name, machine.name),
                )
            })
        })
    };
    let title = move || {
        agent.with(|agent| {
            agent.as_ref().map_or_else(
                || "Remote agent unavailable".into(),
                |agent| agent.name.clone(),
            )
        })
    };
    let headline = NodeRef::<leptos::html::Div>::new();
    headline.on_load(|headline| {
        request_animation_frame(move || {
            let _ = headline.focus();
        });
    });
    Effect::new(move |_| {
        document().set_title(&format!("{} · Minarbot", title()));
    });
    let end = NodeRef::<leptos::html::Div>::new();
    let last_reply = Memo::new(move |_| {
        history.data.with(|data| {
            data.as_ref().map(|data| {
                (
                    data.messages.len(),
                    data.messages.last().and_then(|item| item.reply.clone()),
                )
            })
        })
    });
    Effect::new(move |_| {
        last_reply.track();
        if let Some(end) = end.get() {
            end.scroll_into_view_with_scroll_into_view_options(&{
                let options = web_sys::ScrollIntoViewOptions::new();
                options.set_block(web_sys::ScrollLogicalPosition::Nearest);
                options
            });
        }
    });
    let online = move || {
        machine.with(|machine| {
            machine
                .as_ref()
                .is_some_and(|machine| machine.status == "online")
        })
    };
    let retry = move || pending.with(Option::is_some);
    let icons = use_context::<Icons>().unwrap_or_default();
    view! {
        <main class="chat bs-root remote-conversation" aria-label=move || agent.with(|agent| machine.with(|machine| {
            agent.as_ref().zip(machine.as_ref()).map_or_else(|| "Remote agent unavailable".into(), |(agent, machine)| format!("Conversation {} on {}", agent.name, machine.name))
        }))>
            <header class="chat__header"><span class="chat__pill-anchor">
                <div node_ref=headline tabindex="-1" class="chat__pill" title=move || machine.with(|machine| machine.as_ref().map(|machine| machine.origin.clone()))>
                    {move || avatar(None, agent.with(|agent| agent.as_ref().map_or_else(|| "Remote agent".into(), |agent| agent.name.clone())).as_str(), false, 24)}
                    <span class="chat__pill-name">{title}</span>
                </div>
            </span><div class="chat__header-right">
                <span class="chat__remote-context">{move || machine.with(|machine| machine.as_ref().map_or_else(|| "on unknown machine".into(), |machine| format!("on {}", machine.name)))}</span>
                <span class="activity-chip" class:is-working=online role="status">{move || machine.with(|machine| machine.as_ref().map_or("Unavailable", |machine| machine_status(&machine.status)).to_owned())}</span>
            </div></header>
            <div class="transcript-wrap"><section class="transcript" aria-label="Conversation history" aria-busy=move || history.data.with(Option::is_none).to_string()>
                <div class="transcript__col">
                    <Show when=move || !history.error.with(String::is_empty)>
                        <p class="chat-notice is-error" role="alert">{history.error}" History will be retried."</p>
                    </Show>
                    <Show when=move || machine.with(|machine| machine.as_ref().is_some_and(|machine| !matches!(machine.status.as_str(), "online" | "stale")))>
                        <p class="chat-notice is-warn" role="status">{move || if machine.with(|machine| machine.as_ref().is_some_and(|machine| machine.status == "unauthorized")) {
                            "Peer access was revoked. Pair this machine again in Settings > Machines."
                        } else { "This agent is unavailable. Existing messages remain saved; reconnect the machine before sending." }}</p>
                    </Show>
                    {move || {
                        if directory.data.with(Option::is_none) || agent.with(Option::is_some) && history.data.with(Option::is_none) {
                            return view! { <p class="empty-chat__sub" role="status">"Loading conversation..."</p> }.into_any();
                        }
                        let Some(agent) = agent.get() else {
                            let sub = if machine.with(Option::is_none) { "This machine has been unpaired." } else { "This agent is no longer in the remote directory." };
                            return empty("Remote agent", "Remote agent unavailable", sub.to_owned()).into_any();
                        };
                        let rows = history.data.get().map_or_else(Vec::new, |history| history.messages);
                        if rows.is_empty() {
                            let machine = machine.with(|machine| machine.as_ref().map_or_else(|| "its own machine".into(), |machine| machine.name.clone()));
                            empty(&agent.name, &format!("Message {}", agent.name), format!("Only your message is shared. The agent runs on {machine} and keeps its local approval rules.")).into_any()
                        } else {
                            rows.into_iter().map(|item| turn(item, &agent.name)).collect_view().into_any()
                        }
                    }}
                    <div node_ref=end></div>
                </div>
            </section></div>
            <footer class="composer-area">
                {move || reason.get().map(|reason| view! { <p id="remote-send-reason" class="composer__note" role="status">{reason}</p> })}
                {move || if reason.with(Option::is_some) { None } else {
                    machine.with(|machine| agent.with(|agent| {
                        let (machine, agent) = (machine.as_ref()?, agent.as_ref()?);
                        let account = binding(machine, agent)?.provider_account.as_ref()?;
                        let provider = machine.provider_accounts.as_ref()?.iter().find(|provider| &provider.id == account)?;
                        let model = agent.model.as_ref().filter(|model| !model.is_empty()).or(provider.default_model.as_ref()).cloned();
                        Some(view! { <p class="composer__note">{provider.name.clone()}{model.map(|model| view! { " · "<code>{model}</code> })}</p> })
                    }))
                }}
                <div class="composer__error" role="alert" hidden=move || error.with(String::is_empty)>{error}</div>
                <Show when=move || invalid.get() || retry() && !busy.get()>
                    <div class="composer__error" role="status"><span><strong>{move || if invalid.get() { "Saved message could not be read" } else { "Message submission is not confirmed" }}</strong>
                        ". Check conversation history. Retry keeps the same request ID and text."</span>
                        <kit::Button label=kit::text("Discard saved message") size="sm" variant="ghost" disabled=busy on_click=discard />
                    </div>
                </Show>
                <Show when=move || agent.with(Option::is_some)>
                    <form class="composer" class:is-multiline=move || text.with(|value| value.contains('\n')) on:submit=move |event| { event.prevent_default(); send(); }>
                        <div class="composer__left"><label class="sr-only" for="remote-message">{label}</label>
                            <textarea id="remote-message" name="text" class="composer__input" rows="1" maxlength="16000" required placeholder=label prop:value=text
                                aria-describedby=move || reason.with(Option::is_some).then_some("remote-send-reason")
                                disabled=move || busy.get() || retry() || reason.with(Option::is_some)
                                on:input=move |event| {
                                    let value = event_target_value(&event);
                                    if !draft.with_value(|draft| save(draft, &value)) { error.set("Draft could not be saved in browser storage.".into()); }
                                    text.set(value);
                                }
                                on:keydown=move |event: web_sys::KeyboardEvent| {
                                    if event.key() == "Enter" && !event.shift_key() && !event.is_composing() {
                                        event.prevent_default(); send();
                                    }
                                } />
                        </div>
                        <div class="composer__toolbar">
                            <span class="composer__plus-anchor"><button type="button" class="composer__btn" disabled aria-label="Attach conversation file" title="Remote conversations support text only.">
                                {kit::icon(icons.attach.clone())}</button></span>
                            <div class="composer__right">
                                <span class="composer__retry" hidden=move || !retry() || busy.get()>"Retry send"</span>
                                <button type="submit" class="composer__cta" aria-label=move || if busy.get() { "Sending..." } else if retry() { "Retry send" } else { "Send message" }
                                    title=move || reason.get().unwrap_or_else(|| if retry() { "Retry send".into() } else { "Send (Enter)".into() })
                                    disabled=move || busy.get() || reason.with(Option::is_some) || text.with(|text| text.trim().is_empty())>
                                    <span class="step__spin" aria-hidden="true" hidden=move || !busy.get()></span>
                                    <span class="appearance-icon" aria-hidden="true" hidden=busy inner_html=icons.send.clone()></span>
                                </button>
                            </div>
                        </div>
                    </form>
                </Show>
            </footer>
        </main>
    }
}

fn empty(name: &str, title: &str, description: String) -> impl IntoView {
    view! { <div class="empty-chat">{avatar(None, name, false, 64)}<div class="empty-chat__title">{title.to_owned()}</div><div class="empty-chat__sub">{description}</div></div> }
}
