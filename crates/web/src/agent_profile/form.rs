use super::*;
use crate::connection_ui::{self as ui, query, ErrorNotice, Select, Switch};
use journal::bounded;
use wasm_bindgen::JsCast;

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Runtimes {
    choices: Vec<Runtime>,
    default_runtime: Option<String>,
}

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Runtime {
    runtime: String,
    label: String,
    available: bool,
    enabled: bool,
    reason: Option<String>,
    signed_in: Option<bool>,
    #[serde(default)]
    model: String,
    #[serde(default)]
    models: Vec<Model>,
}

#[derive(Clone, Deserialize, PartialEq)]
struct Model {
    id: String,
    label: String,
    #[serde(default)]
    default: bool,
}

#[derive(Clone, Deserialize, PartialEq)]
struct Models {
    models: Vec<String>,
    #[serde(default)]
    catalog: Vec<Model>,
}

#[derive(Clone, Deserialize, PartialEq)]
struct Account {
    id: String,
    name: String,
    configured: bool,
}

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Login {
    id: String,
    name: String,
    runtime: String,
    signed_in: bool,
}

const CREATION_FIELDS: &[&str] = &[
    "id",
    "name",
    "role",
    "model",
    "instructions",
    "backupModel",
    "threadLimit",
    "toolBudget",
    "runtime",
    "providerAccount",
    "setup",
];

fn valid_base(value: &Value) -> bool {
    let primary = value["model"].as_str().map(str::trim);
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
        && bounded(&value["backupModel"], 160, false)
        && value["backupModel"]
            .as_str()
            .is_some_and(|text| Some(text.trim()) != primary && !text.chars().any(char::is_control))
        && bounded(&value["providerAccount"], 64, false)
        && value["threadLimit"]
            .as_u64()
            .is_some_and(|value| (1..=32).contains(&value))
        && value.get("toolBudget").is_none_or(|value| {
            value
                .as_u64()
                .is_some_and(|value| (10..=200).contains(&value))
        })
        && value.get("runtime").is_none_or(|value| {
            value.as_str().is_some_and(|runtime| {
                matches!(
                    runtime,
                    "api"
                        | "codex"
                        | "claude"
                        | "hermes"
                        | "antigravity"
                        | "grok"
                        | "pi"
                        | "opencode"
                        | "gemini"
                        | "kiro"
                )
            })
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
        "confirmRuntime",
        "harnessModel",
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
                && ["webAccess", "libraryDocsAccess", "learnFromConversations"]
                    .iter()
                    .all(|key| value[*key].is_boolean())
                && ["memoryModel", "goalJudgeModel", "harnessModel"]
                    .iter()
                    .all(|key| {
                        value
                            .get(*key)
                            .is_none_or(|value| bounded(value, 160, false))
                    })
                && value
                    .get("runtime")
                    .is_none_or(|_| value["confirmRuntime"] == true)
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
    #[prop(optional)] models: bool,
    #[prop(optional, into)] suggestions: Signal<bool>,
    #[prop(optional)] placeholder: &'static str,
    #[prop(optional)] changed: Option<Callback<()>>,
) -> impl IntoView {
    let input = NodeRef::<leptos::html::Input>::new();
    let label_for = if id == "backupModel" { "fallback" } else { id };
    view! {
        <div class="k-field">
            <label class="k-field__label" for=id>{label}
              <span class="k-field__optional" hidden=move || !optional.get()>"Optional"</span></label>
            <div class:k-combobox=models>
                <input id=id name=id type="text" node_ref=input class="k-input" autofocus=id == "name" prop:value=value readonly=locked maxlength=max
                  required=required placeholder=placeholder autocomplete="off" list=models.then_some("agent-model-catalog")
                  on:input=move |event| {
                      value.set(event_target_value(&event));
                      if let Some(changed) = changed { changed.run(()); }
                  } />
                <Show when=move || models && !locked.get() && suggestions.get()>
                    <button type="button" class="k-icon-btn k-combobox__toggle" aria-label=format!("Show {label_for} suggestions")
                      on:click=move |_| {
                          if let Some(input) = input.get_untracked() {
                              let _ = input.focus();
                              let _ = input.show_picker();
                          }
                      }><ui::Icon name="chevron-down" /></button>
                </Show>
            </div>
        </div>
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

fn discovers(runtime: &str) -> bool {
    matches!(runtime, "api" | "codex" | "claude")
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
    let runtime = RwSignal::new(initial(
        "runtime",
        profile.runtime.unwrap_or_else(|| "api".into()),
    ));
    let account = RwSignal::new(initial("providerAccount", profile.provider_account));
    let backup = RwSignal::new(initial("backupModel", profile.backup_model));
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
    let docs = RwSignal::new(
        saved["libraryDocsAccess"]
            .as_bool()
            .unwrap_or(profile.library_docs_access != Some(0)),
    );
    let learn = RwSignal::new(
        saved["learnFromConversations"]
            .as_bool()
            .unwrap_or(profile.learn_from_conversations != Some(0)),
    );
    let harness_model = RwSignal::new(saved["harnessModel"].as_str().map(str::to_owned));
    let runtime_chosen = RwSignal::new(editing || !saved.is_null());
    let model_edited = RwSignal::new(editing || !saved.is_null());
    let runtimes = query::<Runtimes>(move || {
        if editing {
            format!("/api/agents/{}/runtimes", id.get_value())
        } else {
            "/api/runtimes".into()
        }
    });
    let accounts = query::<Vec<Account>>(|| "/api/provider-accounts".into());
    let logins = query::<Vec<Login>>(|| "/api/engine-logins".into());
    let models = query::<Models>(move || {
        let runtime = runtime.get_untracked();
        if !discovers(&runtime) {
            return String::new();
        }
        format!(
            "/api/models?runtime={}&account={}",
            ui::encode(&runtime),
            ui::encode(&account.get_untracked())
        )
    });
    Effect::watch(
        move || (runtime.get(), account.get()),
        move |_, _, _| {
            models.data.set(None);
            models.reload.run(());
        },
        false,
    );
    Effect::new(move |_| {
        if !runtime_chosen.get_untracked() {
            runtimes.data.with(|data| {
                if let Some(default) = data.as_ref().and_then(|data| {
                    data.default_runtime.as_ref().filter(|default| {
                        data.choices.iter().any(|choice| {
                            &choice.runtime == *default && choice.available && choice.enabled
                        })
                    })
                }) {
                    runtime.set(default.clone());
                }
            });
        }
    });
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
    let change_runtime = move |next: String| {
        runtime_chosen.set(true);
        if runtime.get_untracked() != next {
            harness_model.set(None);
            goal.set(String::new());
            memory.set(String::new());
            if account.with_untracked(|account| {
                account.starts_with("login:")
                    || account == "device" && !matches!(next.as_str(), "codex" | "claude")
            }) {
                account.set(String::new());
            }
            runtime.set(next);
        }
    };
    let binding_options = Signal::derive(move || {
        let current = account.get();
        let engine = runtime.get();
        let mut options = Vec::new();
        if !accounts
            .data
            .with(|data| data.iter().flatten().any(|account| account.id.is_empty()))
        {
            options.push(("".to_owned(), "Default".to_owned()));
        }
        if matches!(engine.as_str(), "codex" | "claude") {
            let signed_out = runtimes.data.with(|data| {
                data.as_ref().is_some_and(|data| {
                    data.choices
                        .iter()
                        .any(|choice| choice.runtime == engine && choice.signed_in == Some(false))
                })
            });
            options.push((
                "device".into(),
                format!(
                    "{} on this machine{}",
                    if engine == "codex" {
                        "Codex"
                    } else {
                        "Claude Code"
                    },
                    if signed_out { " · Signed out" } else { "" }
                ),
            ));
        }
        accounts.data.with(|data| {
            for account in data.iter().flatten() {
                let name: String = account.name.chars().take(40).collect();
                options.push((
                    account.id.clone(),
                    format!(
                        "{name}{}",
                        if account.configured {
                            ""
                        } else {
                            " · Not connected"
                        }
                    ),
                ));
            }
        });
        logins.data.with(|data| {
            for login in data
                .iter()
                .flatten()
                .filter(|login| login.runtime == engine)
            {
                options.push((
                    format!("login:{}", login.id),
                    format!(
                        "{} login: {}{}",
                        if engine == "codex" { "Codex" } else { "Claude" },
                        login.name,
                        if login.signed_in {
                            ""
                        } else {
                            " · Signed out"
                        }
                    ),
                ));
            }
        });
        if !options.iter().any(|(id, _)| id == &current) {
            options.insert(1, (current, "Unavailable account".into()));
        }
        options
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
    let harness = Signal::derive(move || {
        harness_model.get().unwrap_or_else(|| {
            runtimes.data.with(|data| {
                data.as_ref()
                    .and_then(|data| {
                        data.choices
                            .iter()
                            .find(|choice| choice.runtime == runtime.get())
                            .map(|choice| choice.model.clone())
                    })
                    .unwrap_or_default()
            })
        })
    });
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
        let runtime = runtime.get_untracked();
        let mut body = json!({
            "name": name_value, "role": role.get_untracked().trim(), "model":model_value,
            "instructions":brief, "threadLimit":thread_limit, "toolBudget":tool_budget,
            "backupModel":backup.get_untracked().trim(), "providerAccount":account.get_untracked(),
        });
        if let Some(revision) = revision {
            body["revision"] = json!(revision);
            body["webAccess"] = json!(web.get_untracked());
            body["libraryDocsAccess"] = json!(docs.get_untracked());
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
            if original.with_value(|agent| {
                agent
                    .as_ref()
                    .and_then(|agent| agent.profile.runtime.as_deref())
                    .unwrap_or("api")
                    != runtime
            }) {
                body["runtime"] = json!(runtime);
                body["confirmRuntime"] = json!(true);
            }
            if !discovers(&runtime) {
                body["harnessModel"] = json!(harness.get_untracked());
            }
        } else {
            body["runtime"] = json!(runtime);
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
        content(move || view! { <Menu name="Agent profile actions".to_owned() items><Ellipsis /></Menu> })
    });
    let suggestions = Signal::derive(move || {
        models
            .data
            .with(|data| data.as_ref().is_some_and(|data| !data.models.is_empty()))
    });
    let backup_open = backup.with_untracked(|value| !value.is_empty());
    let judge_open = goal.with_untracked(|value| !value.is_empty());
    let name_label = Signal::derive(move || {
        if !editing && setup.get() {
            "Temporary name".to_owned()
        } else {
            "Name".to_owned()
        }
    });
    let profile_fields = view! {
        <div class="agent-profile__avatar">
            {move || {
                if let Some(look) = look.get() { avatar::mascot(look, 40).into_any() }
                else if let Some(agent) = lifecycle.live(&id.get_value()) { avatar::picture(&agent, 40).into_any() }
                else { view! { <span class="avatar bs-avatar agent-profile__initials" aria-hidden="true">{avatar::initial(&name.get())}</span> }.into_any() }
            }}
            <span class="agent-profile__name">{avatar_title}</span>
            <Button label="Change" disabled=locked click=Callback::new(move |()| tool.set(Some(Tool::Avatar))) />
        </div>
        <Text id="name" label=name_label value=name locked max=80 placeholder="Iris" optional=move || !editing && setup.get() />
        <Text id="role" label=text("Role") value=role locked max=60 optional=true />
        <label class="k-field" for="instructions"><span class="k-field__label">
            {move || if !editing && setup.get() { "Brief" } else { "Instructions" }}
            <span class="k-field__optional" hidden=move || editing || setup.get()>"Optional"</span>
        </span><textarea id="instructions" name="instructions" class="k-input k-textarea" rows="6" maxlength="8000"
            placeholder=move || if !editing && setup.get() { "Describe the work this agent should help with." } else { "" }
            prop:value=instructions readonly=locked on:input=move |event| instructions.set(event_target_value(&event))></textarea>
            {editing.then(|| view! { <span class="agent-profile__count" aria-hidden="true">{move || format!("{}/8000", instructions.with(|text| text.encode_utf16().count()))}</span> })}
        </label>
    };
    let model_fields = view! {
        {agent.as_ref().filter(|agent| agent.profile.runtime.as_deref() == Some("api")).map(|_| view! {
            <Button label="Use Codex" disabled=move || locked.get() || !runtimes.data.with(|data| data.as_ref().is_some_and(|data| data.choices.iter().any(|choice| choice.runtime == "codex" && choice.available && choice.enabled)))
              click=Callback::new(move |()| { change_runtime("codex".into()); if let Some(form) = form.get_untracked() { let _ = form.request_submit(); } }) />
        })}
        <label class="k-field" for="agent-runtime"><span class="k-field__label">"Run with"</span>
            <span class="connection-select agent-profile__runtime-select"><span class="agent-profile__runtime-icon"><ui::Icon name="globe" /></span><select id="agent-runtime" name="runtime" class="k-input k-select" disabled=locked
                on:change=move |event| change_runtime(event_target_value(&event))>
                <option value="api" prop:selected=move || runtime.with(|value| value == "api")>"API model"</option>
                {move || runtimes.data.with(|data| data.iter().flat_map(|data| &data.choices).filter(|choice| choice.runtime != "api").map(|choice| {
                    let id = choice.runtime.clone();
                    let label = if choice.available && choice.enabled { choice.label.clone() } else { format!("{} · {}", choice.label, choice.reason.as_deref().unwrap_or("Unavailable")) };
                    view! { <option value=id.clone() disabled=!choice.available || !choice.enabled prop:selected=move || runtime.with(|value| value == &id)>{label}</option> }
                }).collect_view())}
            </select><span class="k-select__chev"><ui::Icon name="chevron-down" /></span></span>
        </label>
        <ErrorNotice error=runtimes.error.into() retry=runtimes.reload />
        <ui::Field label="Provider" name="agent-provider">
            <Select name="agent-provider" value=account.into() options=binding_options disabled=move || locked.get() || !discovers(&runtime.get())
              change=Callback::new(move |value| { if account.get_untracked() != value { memory.set(String::new()); account.set(value); } }) />
            <ErrorNotice error=accounts.error.into() retry=accounts.reload />
            <ErrorNotice error=logins.error.into() retry=logins.reload />
        </ui::Field>
        <Show when=move || !discovers(&runtime.get())>
            <ui::Field label="Harness model" name="agent-harness-model">
                <Select name="agent-harness-model" value=harness options=Signal::derive(move || runtimes.data.with(|data| {
                    let mut options = vec![("".into(), "Host default".into())];
                    if let Some(choice) = data.as_ref().and_then(|data| data.choices.iter().find(|choice| choice.runtime == runtime.get())) {
                        options.extend(choice.models.iter().map(|model| (model.id.clone(), model.label.clone())));
                    }
                    options
                })) change=Callback::new(move |value| harness_model.set(Some(value))) disabled=locked />
            </ui::Field>
        </Show>
        <div class="k-stack agent-model-field">
            <Text id="model" label=text("Model") value=model locked=Signal::derive(move || locked.get() || !discovers(&runtime.get()))
                required=true models=true suggestions=suggestions placeholder="Enter model ID" changed=Callback::new(move |()| model_edited.set(true)) />
            <datalist id="agent-model-catalog">{move || models.data.with(|data| data.iter().flat_map(|data| data.models.iter().map(|id| {
                let label = data.catalog.iter().find(|item| item.id == *id).map_or(id, |item| &item.label).clone();
                view! { <option value=id.clone() label=label></option> }
            })).collect_view())}</datalist>
            <span class="k-field__hint" role="status" hidden=move || !discovers(&runtime.get()) || !models.busy.get()>"Loading models"</span>
            <ErrorNotice error=models.error.into() retry=models.reload />
        </div>
        {editing.then(|| view! {
            <ui::Disclosure title="Fallback model" open=backup_open>
                <Text id="backupModel" label=text("Fallback model") value=backup locked=Signal::derive(move || locked.get() || runtime.with(|runtime| runtime != "api"))
                    optional=true models=true suggestions=suggestions placeholder="Enter model ID" />
            </ui::Disclosure>
            <ui::Disclosure title="Goal judge model" open=judge_open>
                <Text id="goalJudgeModel" label=text("Goal judge model") value=goal locked optional=true models=true suggestions=suggestions placeholder="Use agent model" />
            </ui::Disclosure>
            <Card>{switch_row("Learn from conversations", learn.into(), locked, Callback::new(move |value| learn.set(value)))}</Card>
            <Text id="memoryModel" label=text("Memory model") value=memory locked optional=true models=true suggestions=suggestions placeholder="Use agent model" />
        })}
    };
    let limits = move || {
        view! {
            {(!editing).then(|| view! {
                <Text id="backupModel" label=text("Fallback model") value=backup locked=Signal::derive(move || locked.get() || runtime.with(|runtime| runtime != "api"))
                    optional=true models=true suggestions=suggestions placeholder="Enter model ID" />
            })}
            {number("threadLimit", "Parallel chats", threads, 1, 32, locked)}
            {number("toolBudget", "Tool step limit", budget, 10, 200, Signal::derive(move || locked.get() || runtime.with(|runtime| runtime != "api")))}
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
                {switch_row("Library docs", Signal::derive(move || web.get() && docs.get()), Signal::derive(move || locked.get() || !web.get()), Callback::new(move |value| docs.set(value)))}
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
                <details class="k-collapsible connection-disclosure" node_ref=advanced open=backup_open>
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
