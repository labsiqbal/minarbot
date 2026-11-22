use super::*;
use crate::system::kit;
use serde::{de, Serialize};

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Answer {
    id: String,
    text: String,
    #[serde(default)]
    acknowledge_uncertain: bool,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    id: String,
    revision: u64,
    decision: DecisionKind,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum DecisionKind {
    Deny,
    Always,
    Allow,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AppCommand {
    path: String,
    body: Decision,
}

fn valid_uuid(id: &str) -> bool {
    protocol::id(id)
        && id.as_bytes()[14] == b'4'
        && matches!(
            id.as_bytes()[19].to_ascii_lowercase(),
            b'8' | b'9' | b'a' | b'b'
        )
}

fn restore<T: de::DeserializeOwned>(
    key: &str,
    limit: usize,
    valid: impl FnOnce(&T) -> bool,
) -> Result<Option<T>, ()> {
    let storage = session().ok_or(())?;
    let Some(raw) = storage.get_item(key).map_err(|_| ())? else {
        return Ok(None);
    };
    if raw.len() > limit {
        return Err(());
    }
    let value = serde_json::from_str(&raw).map_err(|_| ())?;
    valid(&value).then_some(Some(value)).ok_or(())
}

fn request_id(kind: &str, row: RwSignal<Turn>, id: &str) -> String {
    row.with_untracked(|turn| format!("{kind}:{}:{id}", turn.id))
}

fn valid_answer(text: &str, choices: &[String]) -> bool {
    !text.trim().is_empty()
        && text.encode_utf16().count() <= 4000
        && (choices.is_empty() || choices.iter().any(|choice| choice == text.trim()))
}

fn confirm_discard(
    dialog: NodeRef<leptos::html::Dialog>,
    title: &'static str,
    description: &'static str,
    discard: impl Fn() + 'static,
) -> impl IntoView {
    view! {
        <dialog class="k-dialog k-dialog--sm workspace-dialog" node_ref=dialog aria-label=title>
            <div class="k-dialog__head"><h2 class="k-dialog__title">{title}</h2></div>
            <p class="k-dialog__body">{description}</p>
            <form method="dialog" class="k-dialog__foot">
                <button class="k-btn k-btn--md k-btn--secondary" autofocus>"Cancel"</button>
                <button class="k-btn k-btn--md k-btn--danger" on:click=move |_| discard()>"Discard recovery"</button>
            </form>
        </dialog>
    }
}

pub(super) fn question(
    chat: Chat,
    row: RwSignal<Turn>,
    path: String,
    question: Value,
) -> impl IntoView {
    let id = question["id"].as_str().unwrap_or_default().to_owned();
    let card_id = request_id("question", row, &id);
    let label = question["prompt"].as_str().unwrap_or("Question").to_owned();
    let choices = question["choices"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|choice| choice.as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    let answer = question["answer"].as_str().map(str::to_owned);
    let endpoint = StoredValue::new(format!("{path}/questions/{id}/answer"));
    let key = StoredValue::new(format!("minarbot:answer:{id}"));
    let draft_key = StoredValue::new(format!("minarbot:answer:{id}:draft"));
    let pilot = StoredValue::new(format!("minarbot:leptos-action:{}", endpoint.get_value()));
    let recovered = if answer.is_some() {
        let _ = remove(&key.get_value());
        let _ = remove(&pilot.get_value());
        let _ = remove(&draft_key.get_value());
        Ok(None)
    } else {
        let valid =
            |answer: &Answer| valid_uuid(&answer.id) && valid_answer(&answer.text, &choices);
        restore(&key.get_value(), 32_000, valid).and_then(|saved| {
            if saved.is_some() {
                Ok(saved)
            } else {
                restore(&pilot.get_value(), 32_000, valid)
            }
        })
    };
    let invalid = RwSignal::new(recovered.is_err());
    let pending = RwSignal::new(recovered.ok().flatten());
    let draft = RwSignal::new(saved(&draft_key.get_value()).unwrap_or_default());
    let confirmed = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let discard = NodeRef::<leptos::html::Dialog>::new();
    let choices = StoredValue::new(choices);
    let interrupted = move || row.with(|turn| turn.status == "interrupted");
    let unavailable = move || {
        chat.config.with_value(|config| config.archived)
            || row.with(|turn| !matches!(turn.status.as_str(), "queued" | "interrupted"))
    };
    let locked =
        move || unavailable() || pending.with(Option::is_some) || invalid.get() || busy.get();
    let value = move || {
        pending.with(|pending| {
            pending
                .as_ref()
                .map_or_else(|| draft.get(), |answer| answer.text.clone())
        })
    };
    let acknowledged = move || {
        pending.with(|pending| {
            pending
                .as_ref()
                .map_or_else(|| confirmed.get(), |answer| answer.acknowledge_uncertain)
        })
    };
    let change = move |text: String| {
        if !save(&draft_key.get_value(), &text) {
            error.set(
                "Answer draft has not been saved. Copy the text before closing the conversation."
                    .into(),
            );
        }
        draft.set(text);
    };
    let send = move |event: web_sys::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() || invalid.get_untracked() || untrack(unavailable) {
            return;
        }
        let Some(answer) = pending.get_untracked().or_else(|| {
            Some(Answer {
                id: uuid()?,
                text: draft.get_untracked().trim().to_owned(),
                acknowledge_uncertain: confirmed.get_untracked(),
            })
        }) else {
            return;
        };
        if !choices.with_value(|choices| valid_answer(&answer.text, choices))
            || untrack(interrupted) && !answer.acknowledge_uncertain
        {
            return;
        }
        let Ok(raw) = serde_json::to_string(&answer) else {
            return;
        };
        if !save(&key.get_value(), &raw) {
            error.set(
                "Answer could not be saved in the browser. Check storage and try again.".into(),
            );
            return;
        }
        busy.set(true);
        error.set(String::new());
        pending.set(Some(answer));
        let path = endpoint.get_value();
        leptos::task::spawn_local(async move {
            if let Err(reason) = post::<Value>(&path, &raw).await {
                error.try_set(reason.message);
            }
            busy.try_set(false);
        });
    };
    view! {
        <div class="turn-request" id=card_id tabindex="-1">
            <section class="k-stack bs-root"><div class="k-card"><div class="k-stack question-card-body">
                <h3 tabindex="-1" class="k-section__title">{label}</h3>
                {answer.map(|answer| view! { <p>{format!("Answer: {answer}")}</p> }.into_any())
                    .unwrap_or_else(|| view! {
                    <form class="k-stack" on:submit=send>
                        {if choices.with_value(Vec::is_empty) {
                            view! { <label class="k-field"><span class="k-field__label">"Answer"</span>
                                <textarea class="k-textarea" maxlength="4000" required disabled=locked
                                    prop:value=value on:input=move |event| change(event_target_value(&event))></textarea>
                            </label> }.into_any()
                        } else {
                            view! { <fieldset class="k-fieldset" disabled=locked>
                                <legend class="k-field__label">"Answer"</legend>
                                {choices.get_value().into_iter().map(|choice| {
                                    let selected = choice.clone();
                                    let next = choice.clone();
                                    view! { <label class="k-check">
                                        <input type="radio" name=format!("answer-{id}") prop:checked=move || value() == selected
                                            on:change=move |_| change(next.clone()) />
                                        <span class="k-check__label">{choice}</span>
                                    </label> }
                                }).collect_view()}
                            </fieldset> }.into_any()
                        }}
                        <Show when=interrupted>
                            <label class="k-check"><input type="checkbox" class="connection-checkbox" prop:checked=acknowledged disabled=move || busy.get() || pending.with(Option::is_some)
                                on:change=move |event| confirmed.set(event_target_checked(&event)) />
                                <span class="k-check__label">"I have checked the effects of the task before continuing"</span>
                            </label>
                        </Show>
                        <Show when=move || invalid.get()><kit::Alert tone="error">"Answer recovery data is invalid. Discard recovery before continuing."</kit::Alert></Show>
                        <Show when=move || !error.with(String::is_empty)><kit::Alert tone="error">{error}</kit::Alert></Show>
                        <Show when=unavailable><kit::Alert>"Questions cannot be answered while the task is stopped or the conversation is archived."</kit::Alert></Show>
                        <div class="k-row-actions">
                            <button type="submit" class="k-btn k-btn--sm k-btn--primary"
                                disabled=move || busy.get() || invalid.get() || unavailable() || !choices.with_value(|choices| valid_answer(&value(), choices)) || interrupted() && !acknowledged()>
                                {move || if busy.get() { "Sending..." } else if pending.with(Option::is_some) { "Retry answer" } else { "Answer" }}
                            </button>
                            <Show when=move || pending.with(Option::is_some) || invalid.get()>
                                <kit::Button label=kit::text("Discard recovery") size="sm" disabled=busy on_click=move || controls::dialog_open(discard) />
                            </Show>
                        </div>
                    </form>
                    {confirm_discard(discard, "Discard recovery data?", "The answer may already be saved. Check history before sending another answer.", move || {
                        if remove(&pilot.get_value()) && remove(&key.get_value()) { pending.set(None); invalid.set(false); error.set(String::new()); }
                        else { error.set("Recovery data could not be deleted.".into()); }
                    })}
                }.into_any())}
            </div></div></section>
        </div>
    }
}

fn code(label: String, value: String) -> impl IntoView {
    view! { <pre class="k-code" tabindex="0" role="region" aria-label=label><code>{value}</code></pre> }
}

pub(super) fn approval(
    chat: Chat,
    row: RwSignal<Turn>,
    path: String,
    approval: Value,
    harness: bool,
) -> impl IntoView {
    let id = approval["id"].as_str().unwrap_or_default();
    let kind = if harness { "harness" } else { "app" };
    let card_id = request_id(kind, row, id);
    let key = StoredValue::new(format!("minarbot:{kind}-approval:{path}:{id}"));
    let encoded = js_sys::encode_uri_component(id)
        .as_string()
        .unwrap_or_default();
    let endpoint = StoredValue::new(format!("{path}/{kind}-approvals/{encoded}"));
    let pilot = StoredValue::new(format!("minarbot:leptos-action:{}", endpoint.get_value()));
    let open = approval["state"] == "approval";
    let revision = approval["revision"].as_u64().unwrap_or_default();
    let archived = move || chat.config.with_value(|config| config.archived);
    let unavailable = move || {
        archived() || row.with(|turn| turn.status != if harness { "running" } else { "queued" })
    };
    let valid = |decision: &Decision| {
        valid_uuid(&decision.id)
            && decision.revision >= 1
            && (!harness || decision.revision == revision)
    };
    let restored = if !open {
        let _ = remove(&key.get_value());
        let _ = remove(&pilot.get_value());
        Ok(None)
    } else if harness {
        restore(&key.get_value(), 4000, valid)
    } else {
        restore(&key.get_value(), 4000, |command: &AppCommand| {
            valid(&command.body) && endpoint.with_value(|endpoint| endpoint == &command.path)
        })
        .map(|command| command.map(|command| command.body))
    };
    let restored = restored.and_then(|saved| {
        if saved.is_some() || !open {
            Ok(saved)
        } else {
            restore(&pilot.get_value(), 4000, valid)
        }
    });
    let invalid = RwSignal::new(restored.is_err());
    let pending = RwSignal::new(restored.ok().flatten());
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let discard = NodeRef::<leptos::html::Dialog>::new();
    let heading = NodeRef::<leptos::html::H3>::new();
    let disabled = move || busy.get() || invalid.get() || unavailable();
    let decide = move |decision| {
        if !open || untrack(disabled) {
            return;
        }
        let Some(command) = pending.get_untracked().or_else(|| {
            Some(Decision {
                id: uuid()?,
                revision,
                decision,
            })
        }) else {
            return;
        };
        let raw = if harness {
            serde_json::to_string(&command)
        } else {
            serde_json::to_string(&AppCommand {
                path: endpoint.get_value(),
                body: command.clone(),
            })
        };
        if !raw.is_ok_and(|raw| save(&key.get_value(), &raw)) {
            error.set("Decision could not be saved. Check browser storage.".into());
            return;
        }
        pending.set(Some(command.clone()));
        busy.set(true);
        error.set(String::new());
        let path = endpoint.get_value();
        let journal = key.get_value();
        let old_journal = pilot.get_value();
        leptos::task::spawn_local(async move {
            match crate::api::post::<Value>(&path, &command).await {
                Ok(_) => {
                    if remove(&old_journal) && remove(&journal) {
                        pending.try_set(None);
                        if let Some(heading) = heading.try_get_untracked().flatten() {
                            let _ = heading.focus();
                        }
                    } else {
                        error.try_set("Decision saved. Recovery data could not be removed.".into());
                    }
                }
                Err(message) => {
                    error.try_set(message);
                }
            }
            busy.try_set(false);
        });
    };
    let email = approval["kind"] == "email";
    let craft = approval["kind"] == "craft";
    let prefix = approval["prefix"].as_str().map(str::to_owned);
    let text = |key: &str| approval[key].as_str().unwrap_or_default().to_owned();
    let title = if email {
        "Email draft".into()
    } else if craft {
        "Craft Anything permission".into()
    } else if harness {
        "Coding harness permission".into()
    } else {
        text("tool")
    };
    let label = if email {
        "Email draft approval".into()
    } else if craft {
        "Craft Anything approval".into()
    } else if harness {
        "Coding harness approval".into()
    } else {
        format!("Approval for {title}")
    };
    let outcome = match (approval["state"].as_str(), approval["decision"].as_str()) {
        (Some("approval"), _) => "Approval needed",
        (Some("expired"), _) => "Expired; ask again",
        (_, Some("revoked")) => "App access removed",
        (_, Some("deny")) => "Not allowed",
        (Some("uncertain"), _) => "Result uncertain",
        (Some("running"), _) => "Running approved action",
        _ if email => "Draft saved",
        (_, Some("always")) if harness => "Always allowed for this command prefix",
        (_, Some("always")) => "Always allowed for this agent",
        (Some("done"), _) => "Allowed once",
        _ if harness => "Allowed once",
        _ => "Approved, waiting to run",
    };
    let toolkit = match approval["toolkit"].as_str().unwrap_or_default() {
        "gmail" => "Gmail",
        "slack" => "Slack",
        "github" => "GitHub",
        "googlecalendar" => "Google Calendar",
        name => name,
    };
    let account = (!harness).then(|| format!("{toolkit} · {}", text("account")));
    let args = &approval[if harness { "input" } else { "arguments" }];
    let summary = if harness {
        text("description")
    } else {
        let lines = args
            .as_object()
            .into_iter()
            .flatten()
            .take(6)
            .map(|(key, value)| {
                let shown = value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned);
                format!("{key}: {}", shown.chars().take(160).collect::<String>())
            })
            .collect::<Vec<_>>();
        if lines.is_empty() {
            "No arguments".into()
        } else {
            lines.join("\n")
        }
    };
    let details = if email {
        let part = |key| args[key].as_str().unwrap_or_default();
        format!(
            "From: {}\nTo: {}\nSubject: {}\n\n{}",
            part("from"),
            part("to"),
            part("subject"),
            part("text")
        )
    } else {
        serde_json::to_string_pretty(args).unwrap_or_default()
    };
    let allow_always = !harness || prefix.is_some();
    let decision_buttons = [
        (DecisionKind::Deny, "Don't allow", "secondary"),
        (
            DecisionKind::Always,
            if harness {
                "Always allow for this agent + command prefix"
            } else {
                "Always allow for this agent"
            },
            "secondary",
        ),
        (
            DecisionKind::Allow,
            if email { "Approve draft" } else { "Allow once" },
            "primary",
        ),
    ];
    let icons = use_context::<Icons>().unwrap_or_default();
    let recovery_title = Signal::<String>::derive(move || {
        if invalid.get() {
            if harness {
                "Saved approval expired. Reload the transcript.".to_owned()
            } else {
                "Approval recovery data is invalid".to_owned()
            }
        } else {
            "Decision is not confirmed".to_owned()
        }
    });
    view! {
        <div class="turn-request" id=card_id tabindex="-1">
        <section class="bs-root k-stack" aria-label=label><div class="k-card"><div class="approval-card-body k-stack">
            <div class="k-row">
                <div class="k-row__main">
                    <span class="k-row__leading" inner_html=icons.shield></span>
                    <span class="k-row__text"><span class="k-row__title"><h3 class="k-section__title" tabindex="-1" node_ref=heading>{title.clone()}</h3></span>
                        {account.map(|account| view! { <span class="k-row__meta">{account}</span> })}
                    </span>
                </div>
                <span class="k-row__trailing">{kit::badge(if open { "warn" } else { "neutral" }, outcome)}</span>
            </div>
            {(!harness).then(|| view! { <p class="k-field__hint approval-card-note">{if open { "This action may change data in your app. Review its arguments before allowing it." } else { outcome }}</p> })}
            <p class="message-plain approval-card-note">{summary}</p>
            {approval["command"].as_str().map(|command| code("Shell command".into(), command.to_owned()))}
            {if email { code("Email preview".into(), details).into_any() } else { view! {
                <kit::Disclosure title=if harness { "Request details" } else { "All arguments" }>
                    {code(if harness { "Harness request arguments".into() } else { format!("Arguments for {title}") }, details)}
                </kit::Disclosure>
            }.into_any() }}
            {prefix.map(|prefix| view! { <kit::Disclosure title="Always allow scope">
                <p class="k-field__hint">"Applies to this agent, runtime, workspace, and literal command prefix: "<code>{prefix}</code>". Switching runtime clears these grants."</p>
            </kit::Disclosure> })}
            {open.then(|| view! {
                {(!harness).then(|| view! { <p class="k-field__hint approval-card-note">"Always allow applies to this action, app account, agent, and conversation scope. Change it in this project’s Connected apps tab, or Settings > Connected apps for direct chats."</p> })}
                <Show when=move || !error.with(String::is_empty)><kit::Alert tone="error">{error}{harness.then_some(" Retry keeps the original decision and request ID.")}</kit::Alert></Show>
                <Show when=move || invalid.get() || pending.with(Option::is_some)>
                    <kit::Alert tone="warn" title=recovery_title actions=kit::content(move || view! {
                        <Show when=move || pending.with(Option::is_some)>
                            <kit::Button label=kit::text("Retry saved decision") size="sm" disabled=Signal::derive(disabled) on_click=move || {
                                if let Some(command) = pending.get_untracked() { decide(command.decision); }
                            } />
                        </Show>
                        {(!harness).then(|| view! { <kit::Button label=kit::text("Discard recovery") size="sm" disabled=busy on_click=move || controls::dialog_open(discard) /> })}
                    }) />
                </Show>
                <div class="k-row-actions">{decision_buttons.into_iter().filter(|(decision, _, _)| allow_always || !matches!(decision, DecisionKind::Always))
                    .map(|(decision, label, variant)| view! { <kit::Button label=kit::text(label) size="sm" variant=variant
                        disabled=Signal::derive(move || disabled() || pending.with(Option::is_some)) on_click=move || decide(decision) /> }).collect_view()}</div>
                {(!harness).then(|| view! { <Show when=unavailable><kit::Alert>"Approval is unavailable while this task is stopped or its conversation is archived."</kit::Alert></Show> })}
                {(!harness).then(|| confirm_discard(discard, "Discard saved decision?", "The decision may already be saved. Check the transcript before making another decision.", move || {
                    if remove(&pilot.get_value()) && remove(&key.get_value()) { pending.set(None); invalid.set(false); error.set(String::new()); }
                    else { error.set("Recovery data could not be removed.".into()); }
                }))}
            })}
        </div></div></section></div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn react_answer_journal_defaults_old_acknowledgement() -> Result<(), serde_json::Error> {
        let raw = r#"{"id":"00000000-0000-4000-8000-000000000001","text":" One "}"#;
        let answer: Answer = serde_json::from_str(raw)?;
        assert!(valid_uuid(&answer.id));
        assert!(valid_answer(&answer.text, &["One".into()]));
        assert!(!answer.acknowledge_uncertain);
        assert!(!valid_answer("Two", &["One".into()]));
        Ok(())
    }
}
