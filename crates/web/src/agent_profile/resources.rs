use super::*;
use crate::connection_ui::{self as ui, query, ErrorNotice, Switch};
use journal::{bounded, command_id};

fn valid_folder(value: &Value) -> bool {
    fields(value, &["id", "revision", "path", "mode"])
        && revision(&value["revision"], 0)
        && (value.get("path").is_none() && value.get("mode").is_none()
            || bounded(&value["path"], 4096, true)
                && matches!(value["mode"].as_str(), Some("read-only" | "read-write")))
}

pub(super) fn folder(agent: String) -> impl IntoView {
    let journal = Journal::new(
        format!("minarbot-agent-folder:{agent}"),
        32_000,
        valid_folder,
    );
    let endpoint = StoredValue::new(format!("/api/agents/{agent}/folder"));
    let folder = query::<Value>(move || endpoint.get_value());
    let choice = RwSignal::new(false);
    let path = RwSignal::new(String::new());
    let mode = RwSignal::new(String::new());
    let version = RwSignal::new(0_u64);
    let confirm = use_context::<Confirm>().unwrap_or_else(|| RwSignal::new(None));
    let locked = Signal::derive(move || journal.locked());
    let save = move |body| {
        journal.run(
            body,
            |command| {
                let mut endpoint = endpoint.get_value();
                if command.get("path").is_none() {
                    endpoint.push_str("/reset");
                }
                (endpoint, command.clone())
            },
            true,
            move |result| {
                folder.invalidate();
                folder.data.set(Some(result));
                choice.set(false);
                folder.reload.run(());
            },
        )
    };
    let choose = move || {
        journal.error.set(String::new());
        folder.data.with_untracked(|value| {
            if let Some(value) = value {
                path.set(if value["source"] == "custom" {
                    value["path"].as_str().unwrap_or_default().into()
                } else {
                    String::new()
                });
                mode.set(value["mode"].as_str().unwrap_or("read-only").into());
                version.set(value["revision"].as_u64().unwrap_or_default());
                choice.set(true);
            }
        });
    };
    ui::poll(15, move || {
        if !journal.busy.get_untracked() {
            folder.reload.run(());
        }
    });
    view! {
        <div class="k-stack">
            {move || folder.data.with(|data| data.as_ref().map(|value| {
                let path = value["path"].as_str().unwrap_or_default().to_owned();
                let access = format!("{}{}", if value["mode"] == "read-only" { "Read only" } else { "Read and write" }, if value["source"] == "server" { " · Set by server" } else { "" });
                let custom = value["source"] == "custom";
                let managed = value["source"] == "server";
                let version = value["revision"].clone();
                view! { <Card><Row title=text("Working folder") meta=content(move || view! { <code class="agent-folder-path">{path}</code><span>{access}</span> })
                    trailing=content(move || (!managed).then(|| view! {
                        <Button label="Choose" size="sm" disabled=locked click=Callback::new(move |()| choose()) />
                        {custom.then(|| view! { <Button label="Reset" size="sm" disabled=locked click=Callback::new(move |()| save(json!({"revision":version}))) /> })}
                    })) /></Card> }
            }))}
            <Show when=move || folder.data.with(Option::is_none) && folder.busy.get()><ui::Loading label="Loading working folder..." count=1 /></Show>
            <ErrorNotice error=folder.error.into() retry=folder.reload />
            <Recovery journal confirm title="Folder change is not confirmed" retry=Callback::new(move |()| save(Value::Null)) refresh=folder.reload />
        </div>
        <Dialog id="agent-folder" title=text("Choose working folder") open=choice
            on_close=move || choice.set(false) locked=journal.busy
            footer=content(move || view! {
                <Button label="Cancel" disabled=journal.busy click=Callback::new(move |()| choice.set(false)) />
                <button type="submit" form="agent-folder-form" class="k-btn k-btn--md k-btn--primary" disabled=move || locked.get() || journal.busy.get()>{move || if journal.busy.get() { "Saving..." } else { "Save folder" }}</button>
            })>
            <form id="agent-folder-form" class="k-fieldset" on:submit=move |event| {
                event.prevent_default(); event.stop_propagation();
                if !journal.locked() { save(json!({"revision":version.get_untracked(),"path":path.get_untracked(),"mode":mode.get_untracked()})); }
            }>
                <ui::Field label="Absolute folder path" name="agent-folder-path">
                    <input id="agent-folder-path" class="k-input" type="text" required maxlength="4096" autocomplete="off" spellcheck="false"
                      readonly=locked prop:value=path on:input=move |event| path.set(event_target_value(&event)) />
                </ui::Field>
                <ui::Field label="Access" name="agent-folder-mode">
                    <ui::Select name="agent-folder-mode" value=mode.into() disabled=locked
                      options=Signal::stored(vec![("read-only".into(), "Read only".into()), ("read-write".into(), "Read and write".into())])
                      change=Callback::new(move |value| mode.set(value)) />
                </ui::Field>
                {error(journal.error)}
            </form>
        </Dialog>
    }
}

fn valid_computer(value: &Value) -> bool {
    revision(&value["revision"], 0)
        && (fields(value, &["id", "revision", "enabled", "sharedProfile"])
            && value["enabled"].is_boolean()
            && value.get("sharedProfile").is_none_or(Value::is_boolean)
            || fields(value, &["id", "revision", "alwaysOn"]) && value["alwaysOn"].is_boolean())
}

pub(super) fn computer(agent: String) -> impl IntoView {
    let journal = Journal::new(format!("minarbot-computer:{agent}"), 512, valid_computer);
    let endpoint = StoredValue::new(format!("/api/agents/{agent}/computer"));
    let status = query::<Value>(move || endpoint.get_value());
    let confirm = use_context::<Confirm>().unwrap_or_else(|| RwSignal::new(None));
    let locked = Signal::derive(move || journal.locked() || status.data.with(Option::is_none));
    let apply = move |mut body: Value| {
        if !body.is_null() {
            body["revision"] = status.data.with_untracked(|value| {
                value
                    .as_ref()
                    .map_or(Value::Null, |value| value["revision"].clone())
            });
        }
        journal.run(
            body,
            |command| (endpoint.get_value(), command.clone()),
            false,
            move |result| {
                status.invalidate();
                status.data.set(Some(result));
            },
        );
    };
    ui::poll(15, move || {
        if !journal.busy.get_untracked() {
            status.reload.run(());
        }
    });
    let location = window().location();
    let setup_link = crate::settings::section_url(
        "/settings/computer",
        &format!(
            "{}{}",
            location.pathname().unwrap_or_default(),
            location.search().unwrap_or_default()
        ),
    );
    let setup_link = StoredValue::new(setup_link);
    view! {
        <div class="bs-root k-stack">
            {move || status.data.with(|data| data.as_ref().map(|value| {
                let enabled = value["enabled"] == true;
                let available = value["available"] == true;
                let always_on = value["alwaysOn"] == true;
                let shared = value["sharedProfile"] == true;
                let limit = value["alwaysOnLimitReached"] == true && !always_on;
                let off = value["state"] == "off";
                let setup = match value["setup"]["state"].as_str() {
                    Some("installing") => "Setting up Computer…",
                    Some("failed") => "Setup failed",
                    Some("no_runtime") => "No container runtime",
                    Some("image_missing") => "Image missing",
                    Some("downloading") => "Downloading image",
                    _ => "Setup required",
                };
                view! {
                    <Card>
                        <Row title=text("Computer") meta=content(move || view! {
                            <Show when=move || journal.busy.get()><span class="k-progress" role="status"><span class="k-spinner" aria-hidden="true"></span>"Saving Computer settings..."</span></Show>
                            {(!available).then(|| view! { <span>{setup}</span><a class="k-link" href=setup_link.get_value()>"Computer setup"</a> })}
                        }) trailing=content(move || view! {
                            <Switch label="Computer" checked=Signal::derive(move || journal.pending.with(|value| value.as_ref().and_then(|value| value["enabled"].as_bool()).unwrap_or(enabled)))
                              disabled=move || locked.get() || !available change=Callback::new(move |enabled| apply(json!({"enabled":enabled}))) />
                        }) />
                        {available.then(|| view! {
                            <Row title=text("Always-on Computer") trailing=content(move || view! {
                                <span tabindex=limit.then_some("0") title=limit.then_some("Computer limit reached. Stop another Computer or turn off its Always-on setting.")>
                                <Switch label="Always-on Computer" checked=Signal::derive(move || journal.pending.with(|value| value.as_ref().and_then(|value| value["alwaysOn"].as_bool()).unwrap_or(always_on)))
                                  disabled=move || locked.get() || !enabled || limit change=Callback::new(move |always_on| apply(json!({"alwaysOn":always_on}))) />
                                </span>
                            }) />
                            {enabled.then(|| view! {
                                <Row title=text("Shared browser profile") meta=content(move || (!off).then_some("Stop Computer to switch profiles"))
                                  trailing=content(move || view! { <Switch label="Shared browser profile" checked=shared disabled=move || locked.get() || !off
                                    change=Callback::new(move |shared| apply(json!({"enabled":true,"sharedProfile":shared}))) /> }) />
                            })}
                        })}
                    </Card>
                }
            }))}
            <Show when=move || status.data.with(Option::is_none)><ui::Loading label="Loading computer status..." count=1 /></Show>
            <ErrorNotice error=status.error.into() retry=status.reload />
            <Recovery journal confirm title="Couldn't confirm the change" retry=Callback::new(move |()| apply(Value::Null)) refresh=status.reload />
        </div>
    }
}

fn valid_site(value: &Value) -> bool {
    fields(
        value,
        &["id", "site", "revision", "name", "source", "delete"],
    ) && (value.get("site").is_none()
        && value.get("revision").is_none()
        && value.get("delete").is_none()
        || command_id(&value["site"]) && revision(&value["revision"], 1))
        && if value.get("delete").is_some() {
            value["delete"] == true && value.get("name").is_none() && value.get("source").is_none()
        } else {
            bounded(&value["name"], 80, true)
                && value["name"]
                    .as_str()
                    .is_some_and(|name| !name.chars().any(char::is_control))
                && value["source"]
                    .as_str()
                    .is_some_and(|source| source.len() <= 262_144 && !source.trim().is_empty())
        }
}

#[derive(Clone, Deserialize, PartialEq)]
struct Site {
    id: String,
    name: String,
    #[serde(default)]
    source: String,
    revision: u64,
    #[serde(default)]
    deleted: u8,
}

#[derive(Clone, Deserialize, PartialEq)]
struct Page {
    sites: Vec<Site>,
    next_after: Option<String>,
}

pub(super) fn sites(
    agent: String,
    close: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let journal = Journal::new(format!("minarbot-sites:{agent}"), 1_600_000, valid_site);
    let endpoint = StoredValue::new(format!("/api/agents/{agent}/sites"));
    let cursors = RwSignal::new(vec![String::new()]);
    let page = query::<Page>(move || {
        format!(
            "{}?after={}",
            endpoint.get_value(),
            cursors.with_untracked(|cursors| ui::encode(cursors.last().map_or("", String::as_str)))
        )
    });
    let editor = RwSignal::new(journal.locked());
    let editing = RwSignal::new(None::<Site>);
    let name = RwSignal::new(String::new());
    let source = RwSignal::new(String::new());
    let notice = RwSignal::new("");
    let loading = RwSignal::new(false);
    let locked = Signal::derive(move || journal.locked() || loading.get());
    let confirm = use_context::<Confirm>().unwrap_or_else(|| RwSignal::new(None));
    let reset = move || {
        editing.set(None);
        name.set(String::new());
        source.set(String::new());
        journal.error.set(String::new());
    };
    let save = move |delete| {
        let mut body = if delete {
            json!({"delete":true})
        } else {
            json!({"name":name.get_untracked().trim(),"source":source.get_untracked()})
        };
        if let Some(site) = editing.get_untracked() {
            body["site"] = json!(site.id);
            body["revision"] = json!(site.revision);
        }
        journal.run(
            body,
            |command| (endpoint.get_value(), command.clone()),
            false,
            move |result| {
                reset();
                notice.set(if result["deleted"] == 1 {
                    "Site deleted."
                } else {
                    "Site saved."
                });
                page.reload.run(());
            },
        );
    };
    let edit = move |id: String| {
        loading.set(true);
        journal.error.set(String::new());
        spawn_local(async move {
            let result = api::read::<Site>(&format!("{}/{id}", endpoint.get_value())).await;
            if loading.try_set(false).is_some() {
                return;
            }
            match result {
                Ok(site) if site.deleted == 0 => {
                    name.set(site.name.clone());
                    source.set(site.source.clone());
                    editing.set(Some(site));
                    editor.set(true);
                }
                Ok(_) => journal
                    .error
                    .set("Site has been deleted. Reload the list.".into()),
                Err(message) => journal.error.set(message),
            }
        });
    };
    let retry = Callback::new(move |()| save(false));
    let reload = Callback::new(move |()| {
        reset();
        page.reload.run(());
    });
    ui::poll(15, move || {
        if !journal.busy.get_untracked() {
            page.reload.run(());
        }
    });
    view! {
        <Dialog id="agent-sites" title=text("Agent sites") open=Signal::stored(true) on_close=close size="lg" locked=journal.busy>
            <div class="bs-root k-stack">
                <header class="k-section__head"><h2 class="k-section__title">"Agent sites"</h2><div class="k-section__actions">
                    <Button label="Create site" icon="plus" size="sm" variant="primary" disabled=locked click=Callback::new(move |()| editor.set(true)) />
                </div></header>
                <ErrorNotice error=page.error.into() retry=page.reload />
                <Show when=move || !editor.get()><Recovery journal confirm title="Changes are not confirmed yet" retry refresh=reload /></Show>
                <Show when=move || page.data.with(Option::is_none) && page.busy.get()><ui::Loading label="Loading sites..." /></Show>
                <span class="sr-only" role="status">{notice}</span>
                {move || page.data.with(|data| data.as_ref().map(|data| {
                    if data.sites.is_empty() { return view! { <ui::Empty title="No sites" icon="external-link" /> }.into_any(); }
                    let sites = data.sites.clone();
                    view! {
                        <Card>{sites.into_iter().filter(|site| crate::workspace_route::valid_id(&site.id)).map(|site| {
                            let id = site.id.clone();
                            let href = format!("/sites/{id}");
                            let name = site.name.clone();
                            let edit_name = format!("Edit {name}");
                            view! { <Row title=text(site.name.clone()) meta=content(move || view! {
                                <a href=href target="_blank" rel="noopener noreferrer">{format!("Open {name}")}</a>
                            }) trailing=content(move || view! { <Button label="Edit" aria=edit_name size="sm" disabled=locked
                                click=Callback::new(move |()| edit(id.clone())) /> }) /> }
                        }).collect_view()}</Card>
                    }.into_any()
                }))}
                <Show when=move || cursors.with(|cursors| cursors.len() > 1) || page.data.with(|data| data.as_ref().is_some_and(|data| data.next_after.is_some()))>
                    <div class="k-pager">
                        <Button label="Previous sites" size="sm" disabled=move || page.busy.get() || cursors.with(Vec::len) < 2
                            click=Callback::new(move |()| { cursors.update(|cursors| { cursors.pop(); }); page.reload.run(()); }) />
                        <Button label="Next sites" size="sm" disabled=move || page.busy.get() || page.data.with(|value| value.as_ref().is_none_or(|value| value.next_after.is_none()))
                            click=Callback::new(move |()| {
                                if let Some(next) = page.data.with_untracked(|value| value.as_ref().and_then(|value| value.next_after.clone())) {
                                    cursors.update(|cursors| cursors.push(next)); page.reload.run(());
                                }
                            }) />
                    </div>
                </Show>
            </div>
        </Dialog>
        <Dialog id="agent-site-editor" title=Signal::derive(move || if editing.with(Option::is_some) { "Replace site contents".to_owned() } else { "New site".to_owned() })
            open=editor on_close=move || editor.set(false) locked=journal.busy
            footer=content(move || view! {
                <Button label="Close" disabled=journal.busy click=Callback::new(move |()| editor.set(false)) />
                <button type="submit" form="agent-site-form" class="k-btn k-btn--md k-btn--primary" disabled=locked>
                    {move || if journal.busy.get() { "Saving..." } else if editing.with(Option::is_some) { "Save changes" } else { "Create site" }}
                </button>
            })>
            <Recovery journal confirm title="Changes are not confirmed yet" retry refresh=reload />
            <form id="agent-site-form" class="k-stack" on:submit=move |event| {
                event.prevent_default(); if !journal.locked() { save(false); }
            }>
                <ui::Field label="Name" name="agent-site-name"><input id="agent-site-name" class="k-input" required maxlength="80" prop:value=name
                  disabled=locked on:input=move |event| name.set(event_target_value(&event)) /></ui::Field>
                <ui::Field label="HTML" name="agent-site-source"><textarea id="agent-site-source" class="k-input k-textarea" required rows="8" maxlength="262144" prop:value=source
                  disabled=locked on:input=move |event| source.set(event_target_value(&event))></textarea></ui::Field>
                <Show when=move || editing.with(Option::is_some)>
                    <Button label="Cancel edit" size="sm" disabled=locked click=Callback::new(move |()| reset()) />
                    <Button label="Delete site" variant="danger" disabled=locked click=Callback::new(move |()| ask(Some(confirm),
                        format!("Delete {}?", name.get_untracked()), "This permanently deletes the site. Its link will stop working.", "Delete site", true, move || save(true))) />
                </Show>
            </form>
        </Dialog>
    }
}
