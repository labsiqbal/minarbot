use super::{api, copy, date, date_time, kit::*, poll, uuid, Query};
use crate::general::Switch;
use leptos::prelude::*;
use serde::Deserialize;
use serde_json::{json, Value};

pub(super) const PATH: &str = "/api/machines";
pub(super) const ENVIRONMENT: &str = "/api/environment";

#[derive(Clone, Deserialize, PartialEq)]
pub(super) struct Environment {
    pub name: String,
    origin: Option<String>,
}

#[derive(Clone, Deserialize, PartialEq)]
pub(super) struct Agent {
    pub status: String,
}

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) struct Machine {
    pub id: String,
    pub name: String,
    origin: String,
    pub status: String,
    pub version: String,
    agent_count: u32,
    pairing_state: Option<String>,
    last_seen: Option<f64>,
    allow_agent_messaging: bool,
    pub allow_updates: bool,
    pub agents: Vec<Agent>,
    provider_accounts: Option<Vec<Account>>,
}

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Account {
    name: String,
    host: String,
    default_model: String,
}

#[derive(Clone, Deserialize, PartialEq)]
pub(super) struct Machines {
    pub machines: Vec<Machine>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Install {
    source: String,
    token: String,
    expires_at: f64,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pairing {
    code: String,
    expires_at: f64,
}

fn status(machine: &Machine) -> impl IntoView {
    let label = match machine.status.as_str() {
        "online" => "Online",
        "stale" => "Stale",
        "offline" => "Offline",
        "unauthorized" => "Unauthorized",
        "version-mismatch" => "Version mismatch",
        _ => "Unavailable",
    };
    let stale = (machine.status == "stale")
        .then_some(machine.last_seen)
        .flatten();
    badge(
        if machine.status == "online" {
            "success"
        } else {
            "warn"
        },
        if let Some(seen) = stale {
            let date = date(seen);
            view! { <span>"Stale since "<time datetime=String::from(date.to_iso_string())>{String::from(date.to_locale_time_string("default"))}</time></span> }.into_any()
        } else {
            label.into_any()
        },
    )
}

fn countdown(expires_at: f64, now: f64) -> String {
    let seconds = ((expires_at - now) / 1000.0).ceil().max(0.0) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn install_command(install: &Install) -> String {
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
    format!(
        "curl -fsSL {} | sh -s -- --source {} --token {}",
        quote(&format!("{}/api/install/script", install.source)),
        quote(&install.source),
        quote(&install.token)
    )
}

#[derive(Clone, Copy, PartialEq)]
enum DialogKind {
    Local,
    Peer,
    Pair,
    Code,
    Install,
}

pub fn panel() -> impl IntoView {
    let confirm = use_context::<Confirm>();
    let busy = RwSignal::new(false);
    let query = Query::<Machines>::new(|| PATH.into(), 5000, busy);
    let environment = Query::<Environment>::new(|| ENVIRONMENT.into(), 5000, busy);
    let message = RwSignal::new(String::new());
    let notice = RwSignal::new(String::new());
    let active = RwSignal::new(None::<DialogKind>);
    let active_peer = RwSignal::new(String::new());
    let peer = move || {
        let id = active_peer.get();
        query.value.with(|value| {
            value.as_ref().and_then(|value| {
                value
                    .machines
                    .iter()
                    .find(|machine| machine.id == id)
                    .cloned()
            })
        })
    };
    let origin = RwSignal::new(String::new());
    let code_input = RwSignal::new(String::new());
    let pair_command = RwSignal::new(None::<Value>);
    let draft_name = RwSignal::new(None::<String>);
    let name = move || {
        draft_name
            .get()
            .or_else(|| {
                environment
                    .value
                    .with(|value| value.as_ref().map(|value| value.name.clone()))
            })
            .unwrap_or_default()
    };
    let now = RwSignal::new(js_sys::Date::now());
    let pairing = RwSignal::new(None::<Pairing>);
    let install = RwSignal::new(None::<Install>);
    let copied = RwSignal::new(false);
    poll(1000, move || {
        if matches!(
            active.get_untracked(),
            Some(DialogKind::Code | DialogKind::Install)
        ) {
            let time = js_sys::Date::now();
            now.set(time);
            if pairing
                .with_untracked(|code| code.as_ref().is_some_and(|code| code.expires_at <= time))
            {
                pairing.set(None);
                copied.set(false);
            }
        }
    });

    let close = move || {
        if busy.get_untracked() {
            return;
        }
        active.set(None);
        pairing.set(None);
        install.set(None);
        copied.set(false);
        code_input.set(String::new());
        pair_command.set(None);
        message.set(String::new());
    };
    let open = move |kind| {
        message.set(String::new());
        copied.set(false);
        active.set(Some(kind));
    };
    let is_open = move |kind| Signal::derive(move || active.get() == Some(kind));
    let select_peer = move |id| {
        active_peer.set(id);
        open(DialogKind::Peer);
    };
    let save_name = move || {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        message.set(String::new());
        let value = untrack(name);
        leptos::task::spawn_local(async move {
            match api::command::<Environment>(ENVIRONMENT, json!({"name": value})).await {
                Ok(value) => {
                    environment.value.try_set(Some(value));
                    draft_name.try_set(None);
                    notice.try_set("Machine name saved.".into());
                }
                Err(error) => {
                    message.try_set(error);
                }
            }
            busy.try_set(false);
        });
    };
    let pair = move || {
        if busy.get_untracked() {
            return;
        }
        let body = if let Some(body) = pair_command.get_untracked() {
            body
        } else {
            let id = match uuid() {
                Ok(id) => id,
                Err(error) => {
                    message.set(error);
                    return;
                }
            };
            json!({"id": id, "origin": origin.get_untracked(), "code": code_input.get_untracked().trim().to_uppercase()})
        };
        pair_command.set(Some(body.clone()));
        message.set(String::new());
        busy.set(true);
        leptos::task::spawn_local(async move {
            match api::post::<Value>(PATH, &body).await {
                Ok(_) => {
                    pair_command.try_set(None);
                    code_input.try_set(String::new());
                    origin.try_set(String::new());
                    active.try_set(None);
                    notice.try_set("Machines are paired in both directions.".into());
                }
                Err(error) => {
                    message.try_set(error);
                }
            }
            busy.try_set(false);
        });
    };
    let unpair = move |machine: Machine| {
        ask(confirm, format!("Unpair {}?", machine.name), "Removes this machine from your list and revokes its access here. Minarbot also asks the other machine to revoke access. If it is offline, remove this pairing there too.", "Unpair machine", true, move || {
            if busy.get_untracked() { return; }
            let id = machine.id.clone();
            let name = machine.name.clone();
            busy.set(true); message.set(String::new());
            leptos::task::spawn_local(async move {
                match api::command::<Value>("/api/machines/remove", json!({"environmentId": id})).await {
                    Ok(_) => { active.try_set(None); notice.try_set(format!("{name} was unpaired.")); },
                    Err(error) => { message.try_set(error); },
                }
                busy.try_set(false);
            });
        });
    };
    let update_policy = move |path, allowed| {
        if busy.get_untracked() {
            return;
        }
        let Some(machine) = untrack(peer) else {
            return;
        };
        busy.set(true);
        message.set(String::new());
        leptos::task::spawn_local(async move {
            if let Err(error) = api::command::<Value>(
                path,
                json!({"environmentId": machine.id, "allowed": allowed}),
            )
            .await
            {
                message.try_set(error);
            }
            busy.try_set(false);
        });
    };
    let create_code = move || {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        message.set(String::new());
        copied.set(false);
        leptos::task::spawn_local(async move {
            match api::command::<Pairing>("/api/pairing-codes", json!({})).await {
                Ok(value) => {
                    now.try_set(js_sys::Date::now());
                    pairing.try_set(Some(value));
                }
                Err(error) => {
                    message.try_set(error);
                }
            }
            busy.try_set(false);
        });
    };
    let create_install = move || {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        message.set(String::new());
        copied.set(false);
        leptos::task::spawn_local(async move {
            match api::command::<Install>("/api/install/tokens", json!({})).await {
                Ok(value) => {
                    now.try_set(js_sys::Date::now());
                    install.try_set(Some(value));
                }
                Err(error) => {
                    message.try_set(error);
                }
            }
            busy.try_set(false);
        });
    };
    let command = Signal::derive(move || {
        let time = now.get();
        install.with(|install| {
            install
                .as_ref()
                .filter(|install| install.expires_at > time)
                .map(install_command)
        })
    });
    let copy_value = move |value: String| {
        leptos::task::spawn_local(async move {
            match copy(&value).await {
                Ok(()) => {
                    message.try_set(String::new());
                    copied.try_set(true);
                }
                Err(error) => {
                    message.try_set(error);
                    copied.try_set(false);
                }
            }
        })
    };
    let page_error = move || {
        let error = if active.with(Option::is_none) {
            message.get()
        } else {
            String::new()
        };
        if !error.is_empty() {
            return error;
        }
        let error = query.error.get();
        if !error.is_empty() {
            error
        } else {
            environment.error.get()
        }
    };

    view! {
      <div class="k-page bs-root machines-page set4-page">
        <Header title="Machines" actions=content(move || view! {
          <Menu name="Add machine".into() class="k-btn k-btn--primary k-btn--md" items=vec![
              MenuItem { label: "Install Minarbot".into(), danger: false, action: Callback::new(move |_| open(DialogKind::Install)) },
              MenuItem { label: "Pair machine".into(), danger: false, action: Callback::new(move |_| open(DialogKind::Pair)) },
              MenuItem { label: "Show pairing code".into(), danger: false, action: Callback::new(move |_| open(DialogKind::Code)) },
            ]>{icon(icons().plus)}"Add machine"{icon(icons().down)}</Menu>
        }) />
        <Show when=move || !page_error().is_empty()>
          <Alert tone="error" actions=content(move || view! {
            <Show when=move || !query.error.with(String::is_empty) || !environment.error.with(String::is_empty)>
              <Button label=text("Retry") size="sm" on_click=move || { query.retry(); environment.retry(); } />
            </Show>
          })>{page_error}</Alert>
        </Show>
        <span class="sr-only" role="status">{notice}</span>
        <Section title="Paired machines">
          {move || match query.value.get() {
            None => skeleton(3, "Loading machines...").into_any(),
            Some(value) if value.machines.is_empty() => empty("No paired machines", icons().monitor_empty).into_any(),
            Some(value) => view! { <Card>{value.machines.into_iter().map(|machine| {
              let selected = machine.clone();
              let detail_id = machine.id.clone();
              let action_id = machine.id.clone();
              let label = machine.name.clone();
              let pending = machine.pairing_state.as_deref() == Some("pending");
              let count = machine.agent_count;
              view! { <Row title=text(machine.name.clone()) leading=icons().monitor
                meta=content(move || view! {
                  {status(&selected)}<span>{format!("{count} {}", if count == 1 { "agent" } else { "agents" })}</span>
                  {pending.then(|| badge("warn", "Pairing incomplete"))}
                })
                action=Callback::new(move |_| select_peer(detail_id.clone()))
                trailing=content(move || view! {
                  <Menu name=format!("Machine actions: {label}") disabled=busy items=vec![MenuItem {
                    label: format!("Unpair {label}"), danger: true, action: Callback::new(move |_| unpair(machine.clone())),
                  }]>{icon(icons().more)}</Menu>
                  <button type="button" class="k-icon-btn" aria-label=format!("Open {label}") on:click=move |_| select_peer(action_id.clone())>{icon(icons().right)}</button>
                }) /> }
            }).collect_view()}</Card> }.into_any(),
          }}
        </Section>
        <Section title="This device"><Card><Row title=Signal::derive(move || environment.value.with(|value| value.as_ref().map_or_else(|| "This device".into(), |value| value.name.clone())))
          leading=icons().monitor meta=content(|| view! { "This device" })
          action=Callback::new(move |_| open(DialogKind::Local))
          trailing=content(move || view! {
            <button type="button" class="k-btn k-btn--ghost k-btn--md" on:click=move |_| open(DialogKind::Local)>"Resources"{icon(icons().right)}</button>
          }) /></Card></Section>
        <Dialog id="machine-local" title=text("This device") open=is_open(DialogKind::Local) size="lg" on_close=close locked=busy>
          <div class="k-stack set4-detail">
            <Show when=move || environment.value.with(Option::is_some) fallback=move || skeleton(1, "Loading this device...")>
              <form class="k-stack" on:submit=move |event| { event.prevent_default(); save_name(); }>
                <Field id="machine-name" label="Name"><input id="machine-name" class="k-input" required maxlength="120" disabled=busy prop:value=name
                  on:input=move |event| draft_name.set(Some(event_target_value(&event))) /></Field>
                <div class="machines-form-actions">
                  <span class="k-field__hint machines-origin">{move || environment.value.with(|value| value.as_ref().and_then(|value| value.origin.clone()))}</span>
                  <button type="submit" class="k-btn k-btn--secondary k-btn--md"
                    disabled=move || busy.get() || environment.value.with(|value| value.as_ref().is_none_or(|value| value.name == name().trim()))>"Save name"</button>
                </div>
              </form>
            </Show>
            {error(message)}
            <Show when=move || active.get() == Some(DialogKind::Local)><HostMetrics /></Show>
          </div>
        </Dialog>
        <Dialog id="machine-detail" title=Signal::derive(move || peer().map_or_else(|| "Machine".into(), |machine| machine.name))
          open=is_open(DialogKind::Peer) on_close=close locked=busy
          footer=content(move || view! { <Button label=text("Unpair machine") variant="danger" disabled=busy on_click=move || if let Some(machine) = untrack(peer) { unpair(machine); } /> })>
          <div class="k-stack set4-detail">
            {move || peer().map(|machine| {
              let current = machine.clone();
              let paired = machine.pairing_state.as_deref() != Some("pending");
              view! { <Card>
                <Row title=text("Address") trailing=content(move || view! { <span class="machines-origin">{machine.origin}</span> }) />
                <Row title=text("Status") trailing=content(move || status(&current).into_view()) />
                <Row title=text("Version") trailing=content(move || machine.version.into_view()) />
                <Row title=text("Agents") trailing=content(move || machine.agent_count.into_view()) />
                {(!paired).then(|| view! { <Row title=text("Pairing") trailing=content(|| view! { <span title="The other machine has not confirmed return access.">{badge("warn", "Pairing incomplete")}</span> }) /> })}
                <Row title=text("Last seen") trailing=content(move || machine.last_seen.map_or_else(|| "Not seen yet".into(), |value| date_time(value, "default")).into_view()) />
              </Card> }
            })}
            <Card>
              <Row title=text("Incoming messages") trailing=content(move || view! {
                <Switch label="Incoming messages" checked=Signal::derive(move || peer().is_some_and(|machine| machine.allow_agent_messaging)) disabled=busy
                  on_change=move |allowed| update_policy("/api/machines/agent-messaging", allowed) />
              }) />
              <Row title=text("Remote updates") trailing=content(move || view! {
                <Switch label="Remote updates" checked=Signal::derive(move || peer().is_some_and(|machine| machine.allow_updates)) disabled=busy
                  on_change=move |allowed| update_policy("/api/machines/updates", allowed) />
              }) />
            </Card>
            <Section title="Provider accounts">
              {move || match peer().and_then(|machine| machine.provider_accounts) {
                None => empty("Provider accounts unavailable", String::new()).into_any(),
                Some(accounts) if accounts.is_empty() => empty("No provider accounts", String::new()).into_any(),
                Some(accounts) => view! { <Card>{accounts.into_iter().map(|account| view! {
                  <Row title=text(account.name) meta=content(move || view! { <span class="machines-origin">{account.host}</span> })
                    trailing=content(move || if account.default_model.is_empty() { "No default model".into_any() } else { view! { <code>{account.default_model}</code> }.into_any() }) />
                }).collect_view()}</Card> }.into_any(),
              }}
            </Section>
            {error(message)}
          </div>
        </Dialog>
        <Dialog id="machines-pair" title=text("Pair machine") open=is_open(DialogKind::Pair) on_close=close locked=busy
          footer=content(move || view! {
            <Button label=text("Cancel") disabled=busy on_click=close />
            <button form="set4-pair" type="submit" class="k-btn k-btn--primary k-btn--md" disabled=busy>"Pair machine"</button>
          })>
          <form id="set4-pair" class="k-stack set4-detail" on:submit=move |event| { event.prevent_default(); pair(); }>
            <Field id="machine-origin" label="Address"><input id="machine-origin" class="k-input" required type="url" maxlength="2048"
              aria-description="Private HTTPS URL, including port" placeholder="https://workstation.example.test:4317" prop:value=origin disabled=busy
              on:input=move |event| { origin.set(event_target_value(&event)); pair_command.set(None); } /></Field>
            <Field id="machine-code" label="Pairing code"><input id="machine-code" class="k-input" required minlength="12" maxlength="12"
              autocomplete="off" spellcheck="false" prop:value=code_input disabled=busy
              on:input=move |event| { code_input.set(event_target_value(&event).to_uppercase()); pair_command.set(None); } /></Field>
            {error(message)}
          </form>
        </Dialog>
        <Dialog id="machines-code" title=text("Show pairing code") open=is_open(DialogKind::Code) on_close=close locked=busy
          footer=content(move || view! {
            <Button label=text("Close") disabled=busy on_click=close />
            <Show when=move || pairing.with(Option::is_none)><Button label=text("Create code") variant="primary" busy=busy on_click=create_code /></Show>
          })>
          <div class="k-stack set4-detail">
            <Field id="pairing-address" label="Address"><input id="pairing-address" class="k-input" readonly
              prop:value=move || environment.value.with(|value| value.as_ref().and_then(|value| value.origin.clone()).unwrap_or_default()) /></Field>
            {move || match pairing.get() {
              None => empty("No active code", String::new()).into_any(),
              Some(value) => {
                let code = value.code.clone();
                view! {
                  <div class="machines-code" role="region" aria-label="One-time pairing code">
                    <span class="machines-code__value">{value.code}</span>
                    <button type="button" class="k-icon-btn" aria-label=move || if copied.get() { "Copied" } else { "Copy pairing code" }
                      on:click=move |_| copy_value(code.clone())>{move || icon(if copied.get() { icons().check } else { icons().copy })}</button>
                    <span class="k-field__hint" title="Single use. Closing hides this code.">"Expires in "<span class="machines-countdown">{move || countdown(value.expires_at, now.get())}</span></span>
                    <Button label=text("Hide code") size="sm" on_click=move || { pairing.set(None); copied.set(false); } />
                  </div>
                }.into_any()
              },
            }}
            {error(message)}
          </div>
        </Dialog>
        <Dialog id="machines-install" title=text("Install Minarbot") open=is_open(DialogKind::Install) on_close=close locked=busy
          footer=content(move || view! {
            <Button label=text("Close") disabled=busy on_click=close />
            <Show when=move || command.with(Option::is_none)>
              <Button label=Signal::derive(move || if install.with(Option::is_some) { "Create new command".to_owned() } else { "Create install command".into() }) variant="primary" busy=busy on_click=create_install />
            </Show>
          })>
          <div class="k-stack machine-install set4-detail">
            {hint("Linux and macOS requirements", "Requires curl, Python 3, and OpenSSL 3. Both machines need reachable HTTPS addresses configured in MINARBOT_ALLOWED_ORIGINS.")}
            {error(message)}
            <Show when=move || install.with(Option::is_some) && command.with(Option::is_none)><Alert tone="warn">"This install command expired. Create a new one to continue."</Alert></Show>
            <Show when=move || command.with(Option::is_some)>
              <Field id="install-command" label="Install command"><textarea id="install-command" class="k-input k-textarea machine-install__command"
                readonly spellcheck="false" autocomplete="off" prop:value=move || command.get().unwrap_or_default() /></Field>
              <div class="machines-form-actions">
                {hint("Private, single use", "Grants one pairing code. Closing hides the command. Provider credentials stay on each machine.")}
                <span class="k-field__hint">"Expires in "<span class="machines-countdown">{move || install.with(|install| install.as_ref().map(|value| countdown(value.expires_at, now.get())))}</span></span>
                <Button label=Signal::derive(move || if copied.get() { "Copied".to_owned() } else { "Copy command".into() })
                  on_click=move || if let Some(value) = command.get_untracked() { copy_value(value); } />
              </div>
              <span class="sr-only" role="status">{move || if copied.get() { "Command copied." } else { "" }}</span>
            </Show>
          </div>
        </Dialog>
      </div>
    }
}

#[component]
fn HostMetrics() -> impl IntoView {
    let query = Query::<Value>::new(|| "/api/host-metrics".into(), 10_000, RwSignal::new(false));
    let read = move |path: &'static str| {
        query.value.with(|value| {
            value
                .as_ref()
                .and_then(|value| value.pointer(path))
                .cloned()
                .unwrap_or(Value::Null)
        })
    };
    let row = move |label, path| {
        view! {
            <Row title=text(label) trailing=content(move || view! { {move || bytes(read(path))} }) />
        }
    };
    let percent = |value: Value| {
        value
            .as_f64()
            .map_or_else(|| "Unavailable".into(), |value| format!("{value}%"))
    };
    view! {
      <Section title="System resources" actions=content(move || view! {
        <button type="button" class="k-icon-btn" aria-label="Reload metrics" on:click=move |_| query.retry()>{icon(icons().refresh)}</button>
      })>
        {error(query.error)}
        <Show when=move || query.value.with(Option::is_some) fallback=move || view! {
          <Show when=move || query.error.with(String::is_empty)><Card><Row title=text("System resources") trailing=content(|| view! { "Loading..." }) /></Card></Show>
        }>
          <span class="k-field__hint" title="Server host, separate from the browser. Refreshes every 10 seconds while visible.">
            {move || format!("{} · {}", date_time(read("/sampledAt").as_f64().unwrap_or_default(), "en-US"), read("/platform").as_str().unwrap_or_default())}
          </span>
          <Show when=move || read("/filesystem/pressure").as_str().is_some_and(|pressure| pressure != "normal")>
            <Alert tone="error">{move || if read("/filesystem/pressure") == "critical" { "Storage space is critically low." } else { "Storage is almost full." }}" Check free space on the host before adding data."</Alert>
          </Show>
          <Disclosure title="CPU" open=true><Card>
            <Row title=text("Host CPU") trailing=content(move || view! { {move || if read("/cpu/intervalMs").is_null() && read("/cpu/cores").as_u64().unwrap_or_default() > 0 { "Waiting for two readings".into() } else { percent(read("/cpu/percent")) }} }) />
            <Row title=text("Logical cores") trailing=content(move || view! { {move || read("/cpu/cores").as_u64()} }) />
            <Row title=text("Minarbot process CPU") trailing=content(move || view! { {move || if read("/cpu/intervalMs").is_null() { "Waiting for two readings".into() } else { percent(read("/process/cpuPercent")) }} }) />
          </Card></Disclosure>
          <Disclosure title="Memory"><Card>
            <Row title=text("Available host memory") trailing=content(move || view! { {move || format!("{} of {}", bytes(read("/memory/available")), bytes(read("/memory/total")))} }) />
            {row("Minarbot process memory", "/process/rss")}
            <Row title=text("OS process memory limit") trailing=content(move || view! { {move || { let value = read("/process/memoryLimit"); if value.is_null() { "Unlimited or unknown".into() } else { bytes(value) } }} }) />
            {row("Memory available to the process", "/process/availableMemory")}
          </Card></Disclosure>
          <Disclosure title="Storage"><Card>
            {move || if read("/filesystem").is_null() {
                view! { <Row title=text("Filesystem size") trailing=content(|| view! { "Unavailable" }) /> }.into_any()
            } else { view! {
                {row("Available", "/filesystem/available")}
                <Row title=text("Used") trailing=content(move || view! { {move || format!("{} · {}", bytes(read("/filesystem/used")), percent(read("/filesystem/percent")))} }) />
                <Row title=text("Capacity") trailing=content(move || view! { {move || match read("/filesystem/pressure").as_str() {
                    Some("normal") => badge("success", "Normal"),
                    Some("critical") => badge("warn", "Critically low"),
                    _ => badge("warn", "Almost full"),
                }} }) />
            }.into_any()}}
          </Card></Disclosure>
          <Disclosure title="Database"><Card>
            {row("SQLite database pages", "/storage/databaseBytes")}
            {row("Reusable internal space", "/storage/reusableBytes")}
            {move || read("/storage/files").as_array().map(|files| files.iter().map(|file| {
                let name = match file["kind"].as_str().unwrap_or_default() {
                    "database" => "Database", "wal" => "WAL journal", "shared-memory" => "SQLite shared memory",
                    "rollback-journal" => "Rollback journal", "mcp-key" => "MCP keys", kind => kind,
                };
                let value = match file["status"].as_str() {
                    Some("present") => bytes(file["bytes"].clone()), Some("missing") => "File missing".into(),
                    Some("not-regular") => "Not a regular file; not followed".into(), _ => "Unreadable".into(),
                };
                view! { <Row title=text(name) trailing=content(move || value.into_view()) /> }
            }).collect_view())}
          </Card></Disclosure>
        </Show>
      </Section>
    }
}

fn bytes(value: Value) -> String {
    let Some(size) = value
        .as_u64()
        .or_else(|| value.as_str()?.parse::<u64>().ok())
    else {
        return "Unavailable".into();
    };
    let units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let unit = ((63_u32.saturating_sub(size.leading_zeros())) / 10).min(5) as usize;
    let scale = 1024_u128.pow(unit as u32);
    let tenths = u128::from(size) * 10 / scale;
    let number = if tenths.is_multiple_of(10) {
        (tenths / 10).to_string()
    } else {
        format!("{}.{}", tenths / 10, tenths % 10)
    };
    format!("{} {}", super::number(Value::String(number)), units[unit])
}
