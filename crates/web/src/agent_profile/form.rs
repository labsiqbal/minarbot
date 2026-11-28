use super::*;
use crate::connection_ui::{self as ui, query, ErrorNotice, Switch};
use journal::bounded;
use wasm_bindgen::JsCast;

#[derive(Clone, Deserialize, PartialEq)]
struct Model {
    id: String,
    label: String,
    #[serde(default)]
    default: bool,
    #[serde(default)]
    options: Vec<ModelOption>,
}

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ModelOption {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    values: Vec<String>,
}

fn option_label(id: &str) -> String {
    match id {
        "reasoningEffort" => "Reasoning effort".into(),
        "serviceTier" => "Service tier".into(),
        "fastMode" => "Fast mode".into(),
        _ => id
            .chars()
            .enumerate()
            .fold(String::new(), |mut label, (index, ch)| {
                if index > 0 && ch.is_uppercase() {
                    label.push(' ');
                }
                if index == 0 {
                    label.extend(ch.to_uppercase());
                } else {
                    label.extend(ch.to_lowercase());
                }
                label
            }),
    }
}

fn value_label(value: &str) -> String {
    option_label(value)
}

#[derive(Clone, Deserialize, PartialEq)]
struct Models {
    models: Vec<String>,
    #[serde(default)]
    catalog: Vec<Model>,
    #[serde(rename = "memoryDefault")]
    memory_default: Option<MemoryDefault>,
}

#[derive(Clone, Deserialize, PartialEq)]
struct MemoryDefault {
    model: String,
    replaces: Option<Vec<String>>,
}

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ProviderInstance {
    id: String,
    display_name: String,
    kind: String,
    #[serde(default)]
    models: Vec<Model>,
    #[serde(default)]
    model_ids: Vec<String>,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    installed: bool,
    #[serde(default)]
    auth: Option<AuthStatus>,
    #[serde(default)]
    status: String,
}

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct AuthStatus {
    status: String,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Clone, Deserialize, PartialEq, Default)]
struct ProviderInstances {
    providers: Vec<ProviderInstance>,
}

const CREATION_FIELDS: &[&str] = &[
    "id",
    "name",
    "role",
    "model",
    "instructions",
    "threadLimit",
    "toolBudget",
    "providerInstance",
    "modelOptions",
    "setup",
];

fn valid_base(value: &Value) -> bool {
    bounded(&value["name"], 80, true)
        && bounded(&value["model"], 160, true)
        && bounded(&value["instructions"], 8000, true)
        && value.get("role").is_none_or(|role| {
            bounded(role, 60, false)
                && role.as_str().is_some_and(|text| {
                    !text
                        .chars()
                        .any(|ch| ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}'))
                })
        })
        && value
            .get("providerInstance")
            .is_none_or(|instance| bounded(instance, 64, false))
        && value
            .get("modelOptions")
            .is_none_or(|options| options.is_object())
        && value["threadLimit"]
            .as_u64()
            .is_some_and(|value| (1..=32).contains(&value))
        && value.get("toolBudget").is_none_or(|value| {
            value
                .as_u64()
                .is_some_and(|value| (10..=200).contains(&value))
        })
}

fn valid_creation(value: &Value) -> bool {
    fields(value, CREATION_FIELDS)
        && valid_base(value)
        && value.get("setup").is_none_or(Value::is_boolean)
}

fn valid_edit(value: &Value) -> bool {
    let mut allowed = CREATION_FIELDS.to_vec();
    allowed.extend([
        "revision",
        "webAccess",
        "libraryDocsAccess",
        "learnFromConversations",
        "memoryModel",
        "goalJudgeModel",
    ]);
    let pilot_change = fields(
        value,
        &["id", "revision", "name", "role", "model", "instructions"],
    ) && bounded(&value["name"], 80, true)
        && bounded(&value["role"], 80, false)
        && bounded(&value["model"], 160, true)
        && bounded(&value["instructions"], 8000, true);
    revision(&value["revision"], 1)
        && (pilot_change
            || (fields(value, &allowed)
                && valid_base(value)
                && value.get("libraryDocsAccess").is_none_or(Value::is_boolean)
                && ["webAccess", "learnFromConversations"]
                    .iter()
                    .all(|key| value[*key].is_boolean())
                && ["memoryModel", "goalJudgeModel"]
                    .iter()
                    .all(|key| {
                        value
                            .get(*key)
                            .is_none_or(|value| bounded(value, 160, false))
                    })
                && value.get("setup").is_none()))
}

#[component]
fn Text(
    id: &'static str,
    #[prop(into)] label: Signal<String>,
    value: RwSignal<String>,
    #[prop(into)] locked: Signal<bool>,
    #[prop(default = 160)] max: u32,
    #[prop(optional, into)] optional: Signal<bool>,
    #[prop(optional)] required: bool,
    #[prop(optional, into)] placeholder: Signal<String>,
    #[prop(optional)] changed: Option<Callback<()>>,
) -> impl IntoView {
    view! {
        <div class="k-field">
            <label class="k-field__label" for=id>{label}
              <span class="k-field__optional" hidden=move || !optional.get()>"Optional"</span></label>
            <input id=id name=id type="text" class="k-input" autofocus=id == "name" prop:value=value readonly=locked maxlength=max
              required=required placeholder=move || placeholder.get() autocomplete="off"
              on:input=move |event| {
                  value.set(event_target_value(&event));
                  if let Some(changed) = changed { changed.run(()); }
              } />
        </div>
    }
}

/// A provider's models as a dropdown, or a text field while the provider
/// lists none. A saved model missing from the list stays selectable.
#[component]
fn ModelSelect(
    id: &'static str,
    #[prop(into)] label: Signal<String>,
    value: RwSignal<String>,
    #[prop(into)] locked: Signal<bool>,
    #[prop(into)] catalog: Signal<Vec<(String, String)>>,
    #[prop(optional)] required: bool,
    #[prop(optional)] optional: bool,
    placeholder: &'static str,
    #[prop(optional)] changed: Option<Callback<()>>,
) -> impl IntoView {
    let changed = Callback::new(move |()| if let Some(changed) = changed { changed.run(()); });
    view! {
        <Show when=move || !catalog.with(Vec::is_empty)
            fallback=move || view! { <Text id label value locked required optional=optional placeholder=text(placeholder) changed=changed /> }>
            <div class="k-field">
                <label class="k-field__label" for=id>{label}
                  <span class="k-field__optional" hidden=!optional>"Optional"</span></label>
                <span class="connection-select">
                    <select id=id name=id class="k-input" required=required disabled=locked prop:value=move || value.get()
                      on:change=move |event| { value.set(event_target_value(&event)); changed.run(()); }>
                        <option value="">{placeholder}</option>
                        {move || {
                            let current = value.get();
                            (!current.is_empty() && !catalog.with(|catalog| catalog.iter().any(|(id, _)| *id == current)))
                                .then(|| view! { <option value=current.clone() selected>{current.clone()}</option> })
                        }}
                        {move || catalog.get().into_iter().map(|(id, label)| {
                            let selected = id.clone();
                            view! { <option value=id prop:selected=move || value.with(|value| *value == selected)>{label}</option> }
                        }).collect_view()}
                    </select>
                    <span class="k-select__chev" aria-hidden="true"><ui::Icon name="chevron-down" /></span>
                </span>
            </div>
        </Show>
    }
}

fn number(
    id: &'static str,
    label: &'static str,
    value: RwSignal<String>,
    min: u64,
    max: u64,
    locked: Signal<bool>,
) -> impl IntoView {
    view! {
        <label class="k-field" for=id><span class="k-field__label">{label}</span>
          <input id=id name=id type="number" class="k-input" readonly=locked min=min.to_string() max=max.to_string()
            step="1" required prop:value=value on:input=move |event| value.set(event_target_value(&event)) />
        </label>
    }
}

fn switch_row(
    label: &'static str,
    checked: Signal<bool>,
    disabled: Signal<bool>,
    change: Callback<bool>,
) -> impl IntoView {
    view! { <Row title=text(label) trailing=content(move || view! { <Switch label=label checked disabled change /> }) /> }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Avatar,
    Availability,
    Export,
    Sites,
}

pub(super) fn agent_form(
    lifecycle: Lifecycle,
    agent: Option<Agent>,
    default_model: String,
) -> impl IntoView {
    let editing = agent.is_some();
    let profile = agent
        .as_ref()
        .map(|agent| agent.profile.clone())
        .unwrap_or_default();
    let id = StoredValue::new(
        agent
            .as_ref()
            .map(|agent| agent.id.clone())
            .unwrap_or_default(),
    );
    let original = StoredValue::new(agent.clone());
    let journal = Journal::new(
        if editing {
            format!("minarbot:leptos-action:/api/agents/{}/edit", id.get_value())
        } else {
            "minarbot-agent-creation".into()
        },
        64_000,
        if editing { valid_edit } else { valid_creation },
    );
    let saved = journal.pending.get_untracked().unwrap_or(Value::Null);
    let initial =
        |key: &str, default: String| saved[key].as_str().map(str::to_owned).unwrap_or(default);
    let setup = RwSignal::new(saved["setup"] == true);
    let name = RwSignal::new(initial(
        "name",
        agent
            .as_ref()
            .map(|agent| agent.name.clone())
            .unwrap_or_default(),
    ));
    let role = RwSignal::new(initial(
        "role",
        agent
            .as_ref()
            .map(|agent| agent.role.clone())
            .unwrap_or_default(),
    ));
    let instructions = RwSignal::new(initial("instructions", profile.instructions));
    let model = RwSignal::new(initial(
        "model",
        if editing {
            profile.model
        } else {
            default_model
        },
    ));
    let provider_instance = RwSignal::new(initial(
        "providerInstance",
        profile.provider_instance,
    ));
    let model_options = RwSignal::new(
        saved["modelOptions"]
            .as_object()
            .cloned()
            .or_else(|| profile.model_options.as_object().cloned())
            .map(Value::Object)
            .unwrap_or_else(|| json!({})),
    );
    let goal = RwSignal::new(initial(
        "goalJudgeModel",
        profile.goal_judge_model.unwrap_or_default(),
    ));
    let memory = RwSignal::new(initial("memoryModel", profile.memory_model));
    let threads = RwSignal::new(
        saved["threadLimit"]
            .as_u64()
            .or(profile.thread_limit)
            .unwrap_or(2)
            .to_string(),
    );
    let budget = RwSignal::new(
        saved["toolBudget"]
            .as_u64()
            .or(profile.tool_budget)
            .unwrap_or(90)
            .to_string(),
    );
    let web = RwSignal::new(
        saved["webAccess"]
            .as_bool()
            .unwrap_or(profile.web_access != Some(0)),
    );
    let learn = RwSignal::new(
        saved["learnFromConversations"]
            .as_bool()
            .unwrap_or(profile.learn_from_conversations != Some(0)),
    );
    let model_edited = RwSignal::new(editing || !saved.is_null());
    let instances = query::<ProviderInstances>(|| "/api/provider-instances".into());
    let models = query::<Models>(move || {
        let instance = provider_instance.get_untracked();
        if instance.is_empty() {
            return String::new();
        }
        format!("/api/provider-instances/{}/models", ui::encode(&instance))
    });
    Effect::watch(
        move || provider_instance.get(),
        move |_, _, _| {
            models.data.set(None);
            models.reload.run(());
        },
        false,
    );
    Effect::new(move |_| {
        if !model_edited.get_untracked() {
            let recommended = models.data.with(|data| {
                data.as_ref().and_then(|data| {
                    data.catalog
                        .iter()
                        .find(|model| model.default)
                        .map(|model| model.id.clone())
                        .or_else(|| data.models.first().cloned())
                })
            });
            if let Some(recommended) = recommended {
                model.set(recommended);
            }
        }
    });
    let tool = RwSignal::new(None::<Tool>);
    let look = RwSignal::new(None::<avatar::Look>);
    let tab = RwSignal::new("profile");
    let advanced = NodeRef::<leptos::html::Details>::new();
    let form = NodeRef::<leptos::html::Form>::new();
    let submit_error = RwSignal::new(String::new());
    let confirm = use_context::<Confirm>().unwrap_or_else(|| RwSignal::new(None));
    let locked = Signal::derive(move || journal.locked());
    let close = move || lifecycle.close();
    let revision = agent.as_ref().map(|agent| agent.revision);
    let finish = move |updated: Value| {
        if editing {
            lifecycle.close();
            lifecycle.refresh(&id.get_value(), true);
        } else if let Some(look) = look.get_untracked() {
            spawn_local(async move {
                let image = avatar::png(look).await;
                let id = updated["id"].as_str().unwrap_or_default();
                let saved = match image {
                    Ok(image) => avatar::save_created(id, image).await,
                    Err(error) => Err(error),
                };
                if saved.is_err() {
                    avatar::record_failure(id);
                }
                lifecycle.start_chat(&updated);
            });
        } else {
            lifecycle.start_chat(&updated);
        }
    };
    let send = move |body| {
        journal.run(
            body,
            |command| {
                (
                    if editing {
                        format!("/api/agents/{}/edit", id.get_value())
                    } else {
                        "/api/agents".into()
                    },
                    command.clone(),
                )
            },
            true,
            finish,
        )
    };
    let submit = move |event: web_sys::SubmitEvent| {
        event.prevent_default();
        if journal.locked() {
            return;
        }
        submit_error.set(String::new());
        let mut name_value = name.get_untracked().trim().to_owned();
        let mut brief = instructions.get_untracked().trim().to_owned();
        let drafting = !editing && setup.get_untracked();
        if name_value.is_empty() && drafting {
            name_value = "New agent".into();
        }
        if brief.is_empty() && !editing && !drafting {
            brief = "Help the user complete their tasks.".into();
        }
        let (thread_limit, tool_budget) = (
            threads.get_untracked().parse::<u64>().ok(),
            budget.get_untracked().parse::<u64>().ok(),
        );
        let model_value = model.get_untracked().trim().to_owned();
        let invalid = if name_value.is_empty() {
            Some(("name", "profile", "Enter a name."))
        } else if brief.is_empty() {
            Some(("instructions", "profile", "Instructions are required."))
        } else if model_value.is_empty() {
            Some(("model", "model", "Model is required."))
        } else if !thread_limit.is_some_and(|value| (1..=32).contains(&value)) {
            Some(("threadLimit", "advanced", "Use 1–32 parallel chats."))
        } else if !tool_budget.is_some_and(|value| (10..=200).contains(&value)) {
            Some(("toolBudget", "advanced", "Use a tool step limit of 10–200."))
        } else {
            None
        };
        if let Some((field, section, message)) = invalid {
            if editing {
                tab.set(section);
            } else if section != "profile" {
                if let Some(advanced) = advanced.get_untracked() {
                    advanced.set_open(true);
                }
            }
            submit_error.set(message.into());
            request_animation_frame(move || {
                if let Some(field) = document()
                    .get_element_by_id(field)
                    .and_then(|field| field.dyn_into::<web_sys::HtmlElement>().ok())
                {
                    let _ = field.focus();
                }
            });
            return;
        }
        let mut body = json!({
            "name": name_value, "role": role.get_untracked().trim(), "model":model_value,
            "instructions":brief, "threadLimit":thread_limit, "toolBudget":tool_budget,
            "providerInstance":provider_instance.get_untracked(), "modelOptions":model_options.get_untracked(),
        });
        if let Some(revision) = revision {
            body["revision"] = json!(revision);
            body["webAccess"] = json!(web.get_untracked());
            body["learnFromConversations"] = json!(learn.get_untracked());
            body["memoryModel"] = json!(memory.get_untracked().trim());
            let changed_goal = goal.get_untracked();
            if original.with_value(|agent| {
                agent
                    .as_ref()
                    .and_then(|agent| agent.profile.goal_judge_model.as_deref())
                    .unwrap_or_default()
                    != changed_goal
            }) {
                body["goalJudgeModel"] = json!(changed_goal.trim());
            }
        } else {
            if drafting {
                body["setup"] = json!(true);
            }
        }
        send(body);
    };
    let cancel_tool = move || tool.set(None);
    let avatar_title = agent
        .as_ref()
        .map(|agent| agent.name.clone())
        .unwrap_or_else(|| "Avatar".into());
    let menu = agent.as_ref().map(|agent| {
        let name = StoredValue::new(agent.name.clone());
        let visibility = if agent.hidden { "Restore agent" } else { "Hide agent" };
        let items = vec![
            MenuItem { label: "Export template".into(), danger: false, action: Callback::new(move |()| tool.set(Some(Tool::Export))) },
            MenuItem { label: visibility.into(), danger: false, action: Callback::new(move |()| tool.set(Some(Tool::Availability))) },
            MenuItem { label: "Delete agent".into(), danger: true, action: Callback::new(move |()| delete(lifecycle, Some(confirm), id, name, submit_error)) },
        ];
        content(move || view! { <Menu name="Agent profile actions".to_owned() items>{icon(icons().more)}</Menu> })
    });
    let catalog = Signal::derive(move || models.data.with(|data| data.as_ref().map(|data| data.models.iter().map(|id| {
        let label = data.catalog.iter().find(|item| item.id == *id).map_or(id, |item| &item.label).clone();
        (id.clone(), label)
    }).collect::<Vec<_>>()).unwrap_or_default()));
    let name_label = Signal::derive(move || if !editing && setup.get() { "Temporary name".to_owned() } else { "Name".to_owned() });
    let profile_fields = view! {
        <div class="agent-profile__avatar">
            {move || {
                if let Some(agent) = lifecycle.live(&id.get_value()) {
                    avatar::picture(&agent, 40).into_any()
                } else {
                    view! { <span class="agent-profile__initials"><span class="avatar bs-avatar" aria-hidden="true">{avatar::initial(&name.get())}</span></span> }.into_any()
                }
            }}
            <span class="agent-profile__name">{avatar_title.clone()}</span>
            <Button label="Change" disabled=locked click=Callback::new(move |()| tool.set(Some(Tool::Avatar))) />
        </div>
        <Text id="name" label=name_label value=name locked max=80 placeholder=text("Iris") optional=move || !editing && setup.get() />
        <Text id="role" label=text("Role") value=role locked max=60 optional=true />
        <label class="k-field" for="instructions"><span class="k-field__label">{move || if !editing && setup.get() { "Brief" } else { "Instructions" }}<span class="k-field__optional" hidden=move || editing || setup.get()>"Optional"</span></span>
            <textarea id="instructions" name="instructions" class="k-input k-textarea" rows="6" maxlength="8000" placeholder=move || if !editing && setup.get() { "Describe the work this agent should help with." } else { "" } prop:value=instructions readonly=locked on:input=move |event| instructions.set(event_target_value(&event))></textarea>
            {editing.then(|| view! { <span class="agent-profile__count" aria-hidden="true">{move || format!("{}/8000", instructions.with(|text| text.encode_utf16().count()))}</span> })}
        </label>
    };
    let goal_open = goal.with_untracked(|value| !value.is_empty());
    let instance_options = Signal::derive(move || instances.data.with(|data| data.as_ref().map(|data| data.providers.iter().map(|provider| {
        let ready = provider.enabled && provider.installed && (provider.kind != "codex" && provider.kind != "claude" || provider.auth.as_ref().is_some_and(|auth| auth.status == "authenticated"));
        let reason = if !provider.enabled { "Disabled".into() } else if !provider.installed { "Not installed".into() } else if !ready { provider.auth.as_ref().and_then(|auth| auth.reason.clone()).unwrap_or_else(|| "Not authenticated".into()) } else { String::new() };
        (provider.id.clone(), provider.display_name.clone(), ready, reason)
    }).collect::<Vec<_>>()).unwrap_or_default()));
    let model_options_for_selection = Signal::derive(move || models.data.with(|data| data.as_ref().and_then(|data| data.catalog.iter().find(|entry| entry.id == model.get()).map(|entry| entry.options.clone())).unwrap_or_default()));
    let model_fields = view! {
        <ui::Field label="Provider" name="provider-instance">
            <span class="connection-select"><select class="k-input" name="providerInstance" aria-label="Provider instance" prop:value=provider_instance disabled=locked
                on:change=move |event| { provider_instance.set(event_target_value(&event)); model.set(String::new()); model_options.set(json!({})); }>
                <option value="">"Choose a provider"</option>
                {move || instance_options.get().into_iter().map(|(id, label, available, reason)| {
                    // Options arrive after the select renders; each marks itself selected.
                    let selected = id.clone();
                    view! { <option value=id prop:selected=move || provider_instance.with(|value| *value == selected) disabled=!available>{if available { label } else { format!("{label} · {reason}") }}</option> }
                }).collect_view()}
            </select><span class="k-select__chev" aria-hidden="true"><ui::Icon name="chevron-down" /></span></span>
            <ErrorNotice error=instances.error.into() retry=instances.reload />
        </ui::Field>
        <div class="k-stack agent-model-field">
            <ModelSelect id="model" label=text("Model") value=model locked=Signal::derive(move || locked.get() || provider_instance.with(|value| value.is_empty())) catalog required=true placeholder="Choose a model" changed=Callback::new(move |()| model_edited.set(true)) />
            <span class="k-field__hint" role="status" hidden=move || !models.busy.get()>"Loading models"</span>
            <ErrorNotice error=models.error.into() retry=models.reload />
        </div>
        <Show when=move || !model_options_for_selection.with(Vec::is_empty)>
            <div class="agent-model-options"><h3>Model options</h3>
                {move || model_options_for_selection.get().into_iter().map(|option| {
                    let key = option.id.clone();
                    if option.kind == "boolean" {
                        let checked = Signal::derive({ let key = key.clone(); move || model_options.with(|value| value.get(&key).and_then(Value::as_bool).unwrap_or(false)) });
                        let change_key = option.id.clone();
                        let label = option_label(&option.id);
                        view! { <ui::Row title=label.clone() trailing=content(move || view! { <Switch label=label.clone() checked disabled=locked change=Callback::new(move |value| model_options.update(|options| options[&change_key] = json!(value))) /> }) /> }.into_any()
                    } else {
                        let values = option.values.clone();
                        let change_key = option.id.clone();
                        let value_key = change_key.clone();
                        let label = option_label(&option.id);
                        view! { <ui::Field label=label.clone() name="model-option"><select class="k-input" aria-label=label.clone() prop:value=move || model_options.with(|options| options.get(&value_key).and_then(Value::as_str).unwrap_or_default().to_owned()) disabled=locked on:change=move |event| model_options.update(|options| options[&change_key] = json!(event_target_value(&event)))>{values.into_iter().map(|value| { let text = value_label(&value); view! { <option value=value.clone()>{text}</option> } }).collect_view()}</select></ui::Field> }.into_any()
                    }
                }).collect_view()}
            </div>
        </Show>
        {editing.then(|| view! {
            <ui::Disclosure title="Goal judge model" open=goal_open>
                <ModelSelect id="goalJudgeModel" label=text("Goal judge model") value=goal locked catalog optional=true placeholder="Use agent model" />
            </ui::Disclosure>
            <Card>{switch_row("Learn from conversations", learn.into(), locked, Callback::new(move |value| learn.set(value)))}</Card>
        })}
    };
    let limits = move || {
        view! {
            {number("threadLimit", "Parallel chats", threads, 1, 32, locked)}
            {number("toolBudget", "Tool step limit", budget, 10, 200, locked)}
        }
    };
    let recovery = view! {
        <Recovery journal confirm title=if editing { "Profile change is not confirmed" } else { "Creation is not confirmed yet" }
            retry=Callback::new(move |()| send(Value::Null)) refresh=Callback::new(move |()| {
                if editing { lifecycle.refresh(&id.get_value(), true); }
            }) />
    };
    let advanced_section = editing.then(|| view! {
        <div class="k-tabs__panel" role="tabpanel" id="agent-panel-advanced" aria-labelledby="agent-tab-advanced" hidden=move || tab.get() != "advanced">
            {limits()}
            <Card>
                {switch_row("Web access", web.into(), locked, Callback::new(move |value| web.set(value)))}
            </Card>
            <Show when=move || tab.get() == "advanced">
                {move || resources::computer(id.get_value())}
                {move || resources::folder(id.get_value())}
            </Show>
            <div class="details-list agent-profile__resources">
                <Show when=move || tab.get() == "advanced">
                    {move || crate::memory::profile_resources(id.get_value())}
                </Show>
                <button type="button" class="panel-row panel-row--link" on:click=move |_| tool.set(Some(Tool::Sites))>
                    <ui::Icon name="globe" /><span class="panel-row__title">"Agent sites"</span><ui::Icon name="chevron-right" />
                </button>
            </div>
        </div>
    });
    let conditional_form = if editing {
        view! {
            <div class="k-tabs">
                <div class="k-tabs__list" role="tablist" aria-label="Agent profile" on:keydown=move |event| crate::settings::move_focus(&event, "[role=tablist]", true)>
                    {[("profile","Profile"),("model","Model"),("advanced","Advanced")].into_iter().map(|(id, label)| view! {
                        <button type="button" id=format!("agent-tab-{id}") aria-controls=format!("agent-panel-{id}")
                          role="tab" class="k-tabs__tab" aria-selected=move || (tab.get() == id).to_string()
                          data-state=move || if tab.get() == id { "active" } else { "inactive" }
                          tabindex=move || if tab.get() == id { 0 } else { -1 } on:click=move |_| tab.set(id)>{label}</button>
                    }).collect_view()}
                </div>
                <form id="agent-form" node_ref=form class="k-fieldset" novalidate on:submit=submit>
                    <div class="k-tabs__panel" role="tabpanel" id="agent-panel-profile" aria-labelledby="agent-tab-profile" hidden=move || tab.get() != "profile">{profile_fields}</div>
                    <div class="k-tabs__panel" role="tabpanel" id="agent-panel-model" aria-labelledby="agent-tab-model" hidden=move || tab.get() != "model">{model_fields}</div>
                    {advanced_section}
                </form>
            </div>
        }.into_any()
    } else {
        view! {
            <form id="agent-form" node_ref=form class="k-fieldset" novalidate on:submit=submit>
                <div class="k-choice" role="radiogroup" aria-label="How to create an agent"
                    on:keydown=move |event| crate::settings::move_focus(&event, "[role=radiogroup]", true)>
                    {[(false, "Manual"), (true, "Draft with AI")].into_iter().map(|(drafting, label)| view! {
                        <button type="button" class="k-choice__opt" class:is-active=move || setup.get() == drafting
                            role="radio" aria-checked=move || (setup.get() == drafting).to_string() tabindex=move || if setup.get() == drafting { 0 } else { -1 }
                            disabled=locked on:click=move |_| setup.set(drafting)><span class="k-choice__title">{label}</span></button>
                    }).collect_view()}
                </div>
                {profile_fields}
                <details class="k-collapsible connection-disclosure" node_ref=advanced open=true>
                    <summary class="k-collapsible__trigger"><span class="k-collapsible__title">"Advanced settings"</span>
                      <span class="k-collapsible__chev"><ui::Icon name="chevron-down" /></span></summary>
                    <div class="k-collapsible__content">{model_fields}{limits()}</div>
                </details>
            </form>
        }.into_any()
    };
    view! {
        <Dialog id="agent-profile" title=text(if editing { "Agent profile" } else { "New agent" })
            open=Signal::stored(true) on_close=close select_input=editing size=if editing { "lg" } else { "md" } locked=journal.busy actions=content(move || menu.map(|menu| menu()))
            footer=content(move || view! {
                <Button label="Cancel" click=Callback::new(move |()| close()) disabled=journal.busy />
                <button type="submit" form="agent-form" class="k-btn k-btn--primary k-btn--md" class:is-busy=journal.busy
                    disabled=move || journal.busy.get() || journal.invalid.get() on:click=move |event| {
                        if journal.pending.with_untracked(Option::is_some) { event.prevent_default(); send(Value::Null); }
                    }>{move || if journal.busy.get() { "Saving..." } else if journal.pending.with(Option::is_some) { if editing { "Retry saved change" } else { "Retry creation" } }
                    else if editing { "Save changes" } else if setup.get() { "Create and draft" } else { "Create agent" }}</button>
            })>
            {recovery}
            <div class="agent-profile" class:agent-profile--edit=editing>
                {conditional_form}
                {error(submit_error)}
            </div>
        </Dialog>
        {move || match tool.get() {
            Some(Tool::Avatar) => original.with_value(|agent| match agent {
                Some(agent) => avatar::editor(lifecycle, agent.clone(), cancel_tool).into_any(),
                None => avatar::picker_dialog(name, look, cancel_tool).into_any(),
            }),
            Some(Tool::Availability) => lifecycle.get(&id.get_value()).map(|agent| availability(lifecycle, agent, Signal::stored(true), cancel_tool, true)).into_any(),
            Some(Tool::Export) => lifecycle.get(&id.get_value()).map(|agent| templates::export(agent, cancel_tool, false)).into_any(),
            Some(Tool::Sites) => resources::sites(id.get_value(), cancel_tool).into_any(),
            None => ().into_any(),
        }}
    }
}
