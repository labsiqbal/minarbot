use super::{api, copy, date_time, discard, kit::*, restore, save, uuid, valid_uuid, Query};
use leptos::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;

const PATH: &str = "/api/mcp-server/peer-tokens";
const KEY: &str = "minarbot:mcp-server-command";

#[derive(Clone, Deserialize)]
pub(crate) struct Scope {
    id: String,
    group: String,
    label: String,
    hint: String,
}

fn label(scope: &Scope) -> &str {
    match scope.id.as_str() {
        "history:read" => "Read token conversations",
        "orchestration" => "Delegate and use app tools",
        "machine:update" => "Update paired machines",
        _ => &scope.label,
    }
}

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Token {
    id: String,
    name: String,
    scopes: Vec<String>,
    last_used: Option<f64>,
    revoked: Option<f64>,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Page {
    items: Vec<Token>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    token: Option<String>,
    already_created: Option<bool>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Command {
    path: String,
    body: Body,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Body {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scopes: Option<Vec<String>>,
}

fn validate(command: &Command, catalog: &[Scope]) -> bool {
    let body = &command.body;
    if !valid_uuid(&body.id) {
        return false;
    }
    if command.path == PATH {
        body.name
            .as_ref()
            .is_some_and(|name| !name.trim().is_empty() && name.encode_utf16().count() <= 80)
            && body.scopes.as_ref().is_some_and(|scopes| {
                !scopes.is_empty()
                    && scopes
                        .iter()
                        .all(|id| catalog.iter().any(|scope| scope.id == *id))
            })
    } else {
        command
            .path
            .strip_prefix(&format!("{PATH}/"))
            .and_then(|path| path.strip_suffix("/revoke"))
            .is_some_and(valid_uuid)
            && body.name.is_none()
            && body.scopes.is_none()
    }
}

fn token_badge(token: &Token) -> impl IntoView {
    if token.revoked.is_none() {
        badge("success", "Active")
    } else {
        badge("neutral", "Revoked")
    }
}

pub fn panel() -> impl IntoView {
    let catalog =
        use_context::<StoredValue<Vec<Scope>>>().unwrap_or_else(|| StoredValue::new(Vec::new()));
    let confirm = use_context::<Confirm>();
    let recovery = restore::<Option<Command>>(KEY)
        .map(Option::flatten)
        .and_then(|value| {
            if value
                .as_ref()
                .is_some_and(|command| !catalog.with_value(|catalog| validate(command, catalog)))
            {
                Err("Invalid MCP server recovery data.".into())
            } else {
                Ok(value)
            }
        });
    let invalid = RwSignal::new(recovery.is_err());
    let pending = RwSignal::new(recovery.ok().flatten());
    let busy = RwSignal::new(false);
    let pause = RwSignal::new(false);
    Effect::new(move |_| pause.set(invalid.get() || pending.with(Option::is_some) || busy.get()));
    let query = Query::<Page>::new(|| PATH.into(), 5000, pause);
    let page = query.value;
    let name = RwSignal::new(String::new());
    let selected = RwSignal::new(vec!["agents:read".to_owned()]);
    let form_open = RwSignal::new(false);
    let detail = RwSignal::new(None::<Token>);
    let secret = RwSignal::new(String::new());
    let message = RwSignal::new(String::new());
    let notice = RwSignal::new(String::new());
    let locked =
        Signal::derive(move || busy.get() || pending.with(Option::is_some) || invalid.get());
    let execute = move |command: Command| {
        if busy.get_untracked() || invalid.get_untracked() {
            return;
        }
        if !catalog.with_value(|catalog| validate(&command, catalog)) {
            message.set("Invalid MCP server recovery data.".into());
            return;
        }
        if let Err(error) = save(KEY, &command) {
            message.set(error);
            return;
        }
        busy.set(true);
        pending.set(Some(command.clone()));
        message.set(String::new());
        notice.set(String::new());
        secret.set(String::new());
        leptos::task::spawn_local(async move {
            let result = api::post::<Receipt>(&command.path, &command.body).await;
            if busy.try_get_untracked().is_none() {
                return;
            }
            match result.and_then(|receipt| {
                discard(KEY)?;
                Ok(receipt)
            }) {
                Ok(receipt) => {
                    pending.set(None);
                    form_open.set(false);
                    name.set(String::new());
                    selected.set(vec!["agents:read".into()]);
                    secret.set(receipt.token.unwrap_or_default());
                    if receipt.already_created == Some(true) {
                        notice.set("Token was already created. Its secret cannot be recovered. Revoke it and create a replacement if you did not save it.".into());
                    }
                    crate::settings::focus_heading();
                }
                Err(error) => {
                    message.set(error);
                }
            }
            busy.set(false);
        });
    };
    let create = move || {
        if locked.get_untracked()
            || name.with_untracked(|value| value.trim().is_empty())
            || selected.with_untracked(Vec::is_empty)
        {
            return;
        }
        match uuid() {
            Ok(id) => execute(Command {
                path: PATH.into(),
                body: Body {
                    id,
                    name: Some(name.get_untracked()),
                    scopes: Some(selected.get_untracked()),
                },
            }),
            Err(error) => message.set(error),
        }
    };
    let revoke = move |token: Token| {
        ask(confirm, format!("Revoke {}?", token.name), "This token will stop working immediately. Work already queued or running will continue.", "Revoke token", true, move || {
            if locked.get_untracked() { return; }
            match uuid() {
                Ok(id) => execute(Command { path: format!("{PATH}/{}/revoke", token.id), body: Body { id, name: None, scopes: None } }),
                Err(error) => message.set(error),
            }
        });
    };
    let clear = move || {
        ask(confirm, "Discard the saved request?", "The change may already have succeeded. Check the token list before creating a replacement.", "Discard request", true, move || {
            match discard(KEY) {
                Ok(()) => { pending.set(None); invalid.set(false); message.set(String::new()); },
                Err(error) => { message.set(error); },
            }
        });
    };
    let copy_value = move |value: String| {
        leptos::task::spawn_local(async move {
            match copy(&value).await {
                Ok(()) => {
                    message.try_set(String::new());
                    notice.try_set("Copied.".into());
                }
                Err(error) => {
                    notice.try_set(String::new());
                    message.try_set(error);
                }
            }
        });
    };
    let endpoint = move || {
        if page.with(Option::is_some) {
            window()
                .location()
                .origin()
                .map(|origin| format!("{origin}/mcp"))
                .unwrap_or_default()
        } else {
            String::new()
        }
    };
    let select_value = move |event: web_sys::FocusEvent| {
        if let Some(input) = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::HtmlInputElement>().ok())
        {
            input.select();
        }
    };
    let error_text = move || {
        let local = message.get();
        if local.is_empty() {
            query.error.get()
        } else {
            local
        }
    };
    let recovery = move || {
        view! {
            <Show when=move || invalid.get()>
              <Alert tone="error" title=text("Recovery unavailable") actions=content(move || view! { <Button label=text("Discard request") size="sm" on_click=clear /> }) />
            </Show>
            <Show when=move || pending.with(Option::is_some)>
              <Show when=move || busy.get() fallback=move || view! {
                <Alert tone="warn" title=text("Not confirmed") actions=content(move || view! {
                  <Button label=text("Try again") size="sm" on_click=move || if let Some(command) = pending.get_untracked() { execute(command); } />
                  <Button label=text("Discard request") size="sm" on_click=clear />
                })>"A created token is never revealed twice."</Alert>
              }><span class="k-progress" role="status"><span class="k-spinner" aria-hidden="true"></span>"Applying change..."</span></Show>
            </Show>
        }
    };
    view! {
      <div class="k-page bs-root bs-set3-external">
        <Header title="External access" actions=content(move || view! {
          <button type="button" class="k-btn k-btn--primary k-btn--md" disabled=locked on:click=move |_| form_open.set(true)>
            {icon(icons().plus)}"Create token"
          </button>
        }) />
        <Show when=move || !error_text().is_empty()>
          <Alert tone="error" actions=content(move || view! { <Button label=text("Retry") size="sm" on_click=move || query.retry() /> })>{error_text}</Alert>
        </Show>
        <Show when=move || notice.with(|value| value == "Copied.")><span role="status" class="sr-only">"Copied."</span></Show>
        <Show when=move || notice.with(|value| !value.is_empty() && value != "Copied.")><Alert>{notice}</Alert></Show>
        {recovery()}
        <Field id="external-endpoint" label="Endpoint"><div class="k-inline">
          <input id="external-endpoint" class="k-input" readonly aria-description="Remote clients need an allowed address." prop:value=endpoint on:focus=select_value />
          <button type="button" class="k-icon-btn" aria-label="Copy endpoint URL" disabled=move || page.with(Option::is_none) on:click=move |_| copy_value(untrack(endpoint))>{icon(icons().copy)}</button>
        </div></Field>
        <Section title="Access tokens">
          {move || match page.get() {
            None if error_text().is_empty() => skeleton(3, "Loading access tokens...").into_any(),
            None => ().into_any(),
            Some(value) if value.items.is_empty() => empty("No access tokens", icons().key_empty).into_any(),
            Some(value) => view! { <Card>{value.items.into_iter().map(|token| {
                let action_token = token.clone();
                let info = token.clone();
                let token_name = token.name.clone();
                let menu_name = token.name.clone();
                let active = token.revoked.is_none();
                view! { <Row title=text(token_name) leading=icons().key
                  meta=content(move || view! {
                    {token_badge(&info)}<span>{format!("{} permissions", info.scopes.len())}</span>
                    <span>{info.last_used.map_or_else(|| "Never used".into(), |seen| format!("Last used {}", date_time(seen, "en-US")))}</span>
                  })
                  action=Callback::new(move |_| detail.set(Some(action_token.clone())))
                  trailing=content(move || if active {
                    let item = MenuItem { label: "Revoke token".into(), danger: true, action: Callback::new(move |_| revoke(token.clone())) };
                    view! { <Menu name=format!("Token actions for {menu_name}") items=vec![item] disabled=locked>{icon(icons().more)}</Menu> }.into_any()
                  } else { icon(icons().right).into_any() }) /> }
            }).collect_view()}</Card> }.into_any(),
          }}
        </Section>
        <Dialog id="external-form" title=text("Create token") open=form_open size="lg" on_close=move || form_open.set(false) locked=busy
          footer=content(move || view! {
            <button type="submit" class="k-btn k-btn--primary k-btn--md" form="create-peer-token" disabled=move || locked.get() || name.with(|value| value.trim().is_empty()) || selected.with(Vec::is_empty)>
              <span class="k-spinner" aria-hidden="true" hidden=move || !busy.get()></span>"Create token"
            </button>
          })>
          {error(message)}{recovery()}
          <form id="create-peer-token" on:submit=move |event| { event.prevent_default(); create(); }>
            <fieldset disabled=locked class="k-fieldset">
              <Field id="external-name" label="Name"><input id="external-name" class="k-input" required maxlength="80" placeholder="Work laptop" aria-description="Shown as the sender of messages from this token."
                prop:value=name on:input=move |event| name.set(event_target_value(&event)) /></Field>
              <div role="group" aria-label="Permissions">
                {["Read", "Write", "Run"].map(|group| view! { <fieldset class="mcp-scope-group"><legend>{group}</legend><div class="k-grid-2">
                    {catalog.with_value(|catalog| catalog.iter().filter(|scope| scope.group == group).cloned().map(|scope| {
                        let id = StoredValue::new(scope.id.clone());
                        let title = label(&scope).to_owned();
                        let selected_now = move || selected.with(|items| id.with_value(|id| items.contains(id)));
                        view! { <div class="k-inline">
                          <label class="k-check system-tooltip">
                            <input type="checkbox" class="k-check__box system-checkbox" prop:checked=selected_now aria-description=scope.hint.clone()
                              data-state=move || if selected_now() { "checked" } else { "unchecked" }
                              on:change=move |event| selected.update(|items| {
                                if event_target_checked(&event) { items.push(id.get_value()); } else { id.with_value(|id| items.retain(|item| item != id)); }
                              }) />
                            <span class="system-checkbox-check">{icon(icons().check)}</span>
                            <span class="k-check__label">{title}</span><span class="k-tooltip" aria-hidden="true">{scope.hint}</span>
                          </label>
                        </div> }
                    }).collect_view())}
                </div></fieldset> })}
              </div>
            </fieldset>
          </form>
        </Dialog>
        <Dialog id="external-secret" title=text("Copy token now") open=Signal::derive(move || !secret.with(String::is_empty)) on_close=|| {} locked=Signal::derive(|| true) hide_header=true
          footer=content(move || view! { <Button label=text("I saved the token") variant="primary" on_click=move || { secret.set(String::new()); notice.set(String::new()); crate::settings::focus_heading(); } /> })>
          <div class="k-stack">
            <h2 class="k-dialog__title" tabindex="-1" autofocus>"Copy token now"</h2>
            <div class="k-inline">{badge("warn", "Shown once")}</div>
            {error(message)}
            <Show when=move || notice.with(|value| value == "Copied.")><span role="status">"Copied."</span></Show>
            <Field id="external-token" label="Token"><div class="k-inline">
              <input id="external-token" class="k-input" data-private-token="" readonly autocomplete="off" spellcheck="false" prop:value=secret on:focus=select_value />
              <button type="button" class="k-icon-btn" aria-label="Copy token" on:click=move |_| copy_value(secret.get_untracked())>{icon(icons().copy)}</button>
            </div></Field>
          </div>
        </Dialog>
        <Dialog id="external-detail" title=Signal::<String>::derive(move || detail.with(|token| token.as_ref().map_or_else(|| "Token permissions".into(), |token| token.name.clone()))) open=Signal::derive(move || detail.with(Option::is_some)) on_close=move || detail.set(None)>
          {move || detail.get().map(|token| view! { <div class="k-stack">
            <div>{token_badge(&token)}</div>
            <Section title="Permissions"><Card>{token.scopes.into_iter().map(|id| {
                let title = catalog.with_value(|catalog| catalog.iter().find(|scope| scope.id == id).map(|scope| label(scope).to_owned())).unwrap_or(id);
                view! { <Row title=text(title) /> }
            }).collect_view()}</Card></Section>
          </div> })}
        </Dialog>
      </div>
    }
}
