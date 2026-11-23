use super::*;
use crate::{connection_ui as ui, settings, system::kit};

#[derive(Clone, Deserialize, PartialEq)]
struct Summary {
    id: String,
    from: String,
    subject: String,
    received: i64,
}

impl Summary {
    fn subject(&self) -> &str {
        if self.subject.is_empty() {
            "(No subject)"
        } else {
            &self.subject
        }
    }
}

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Page {
    address: Option<String>,
    items: Vec<Summary>,
    next_before: Option<i64>,
}

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Message {
    #[serde(flatten)]
    summary: Summary,
    to: String,
    text: String,
    #[serde(default)]
    text_truncated: bool,
}

fn received(value: i64) -> impl IntoView {
    let date = date_at(value);
    let text = date_format(
        &date,
        &js_sys::Array::new(),
        &[("dateStyle", "medium"), ("timeStyle", "short")],
    );
    view! { <time datetime=date.to_iso_string().as_string()>{text}</time> }
}

pub(super) fn page(agent: String) -> impl IntoView {
    let base = StoredValue::new(format!("/api/agents/{agent}/email"));
    let name = Signal::derive({
        let agent = agent.clone();
        move || {
            ui::local_agents()
                .into_iter()
                .find(|item| item.id == agent)
                .map(|item| item.name)
        }
    });
    let back = routes::link(&format!("/agents/{agent}"));
    let settings_url = settings::section_url("/settings/email", &format!("/agents/{agent}/inbox"));
    let cursors = RwSignal::new(Vec::<i64>::new());
    let selected = RwSignal::new(None::<String>);
    let list = ui::query::<Page>(move || {
        base.with_value(|base| {
            cursors.with_untracked(|cursors| {
                cursors
                    .last()
                    .map_or_else(|| base.clone(), |cursor| format!("{base}?before={cursor}"))
            })
        })
    });
    let mail = ui::query::<Message>(move || {
        selected.with_untracked(|selected| {
            selected.as_ref().map_or_else(String::new, |id| {
                base.with_value(|base| format!("{base}/messages/{id}"))
            })
        })
    });
    poll_visible(list.reload, 5000);
    let heading = NodeRef::<leptos::html::H1>::new();
    let reader = NodeRef::<leptos::html::H2>::new();
    heading.on_load(|heading| {
        let _ = heading.focus();
    });
    Effect::new(move |_| {
        if mail.data.with(Option::is_some) || !mail.error.with(String::is_empty) {
            if let Some(reader) = reader.get() {
                let _ = reader.focus();
            }
        }
    });
    Effect::new(move |_| {
        if let Some(name) = name.get() {
            document().set_title(&format!("Inbox · {name} · Minarbot"));
        }
    });
    let notice = RwSignal::new("");
    let choose = move |id: Option<String>| {
        if selected.get_untracked() == id {
            if let Some(reader) = reader.get_untracked() {
                let _ = reader.focus();
            }
            return;
        }
        selected.set(id);
        mail.data.set(None);
        mail.reload.run(());
    };
    let refresh = move || {
        list.reload.run(());
        if selected.with_untracked(Option::is_some) {
            mail.reload.run(());
        }
    };
    let next = move || {
        list.data
            .with(|page| page.as_ref().and_then(|page| page.next_before))
    };
    let navigate = move |older| {
        if older {
            if let Some(cursor) = next() {
                cursors.update(|cursors| cursors.push(cursor));
            }
        } else {
            cursors.update(|cursors| {
                cursors.pop();
            });
        }
        choose(None);
        list.data.set(None);
        list.reload.run(());
        if let Some(heading) = heading.get_untracked() {
            let _ = heading.focus();
        }
    };
    let icon = kit::icons();
    let icons = use_context::<ui::Icons>().unwrap_or_default();
    let inbox_icon = icons.0.get("inbox").cloned().unwrap_or_default();
    view! {
        <main class="chat page-view bs-root"><div class="page-scroll"><div class="page legacy is-wide">
        <div class="k-page bs-root agent-inbox">
            <header class="k-page-head">
                <div class="k-page-head__text"><h1 node_ref=heading tabindex="-1">"Inbox"</h1><p>{name}</p></div>
                <div class="k-page-head__actions">
                    <button type="button" class="k-icon-btn" aria-label="Refresh inbox" title="Refresh inbox" on:click=move |_| refresh()>{kit::icon(icon.refresh)}</button>
                    <a class="k-btn k-btn--md k-btn--ghost" href=back>"Back to chat"</a>
                </div>
            </header>
            {move || list.data.get().map(|page| match page.address {
                Some(address) => view! { <div class="agent-inbox__address"><code>{address.clone()}</code>
                    <button type="button" class="k-icon-btn" aria-label="Copy email address" title="Copy email address" on:click=move |_| {
                        let address = address.clone();
                        leptos::task::spawn_local(async move {
                            let status = if ui::copy(&address).await.is_ok() { "Address copied." } else { "Select the address to copy it." };
                            notice.try_set(status);
                        });
                    }>{kit::icon(icon.copy.clone())}</button>
                    <span class="sr-only" role="status">{notice}</span>
                </div> }.into_any(),
                None => {
                    let href = settings_url.clone();
                    view! { <kit::Alert actions=kit::content(move || view! {
                        <a class="k-btn k-btn--sm k-btn--secondary" href=href>"Email settings"</a>
                    })>"Set a receiving domain to enable new mail."</kit::Alert> }.into_any()
                }
            })}
            {kit::error(list.error)}
            <div class="agent-inbox__layout" class:has-message=move || selected.with(Option::is_some)>
                <div class="agent-inbox__list">
                    {move || match list.data.get() {
                        Some(page) if page.items.is_empty() => kit::empty("No messages", inbox_icon.clone()).into_any(),
                        Some(page) => view! { <kit::Card><ul>{page.items.into_iter().filter(|item| protocol::id(&item.id)).map(|item| {
                            let id = StoredValue::new(item.id.clone());
                            view! { <li><button type="button" id=format!("inbox-message-{}", item.id)
                                aria-current=move || selected.with(|selected| selected.as_ref().is_some_and(|selected| id.with_value(|id| id == selected))).then_some("true")
                                on:click=move |_| choose(Some(id.get_value()))>
                                <strong>{item.subject().to_owned()}</strong><span>{item.from}</span>{received(item.received)}
                            </button></li> }
                        }).collect_view()}</ul></kit::Card> }.into_any(),
                        None if list.error.with(String::is_empty) => kit::skeleton(4, "Loading inbox...").into_any(),
                        None => ().into_any(),
                    }}
                    <div class="k-pager" hidden=move || cursors.with(Vec::is_empty) && next().is_none()>
                        <button type="button" class="k-icon-btn" aria-label="Newer messages" disabled=move || cursors.with(Vec::is_empty) on:click=move |_| navigate(false)>{kit::icon(icon.left.clone())}</button>
                        <button type="button" class="k-icon-btn" aria-label="Older messages" disabled=move || next().is_none() on:click=move |_| navigate(true)>{kit::icon(icon.right)}</button>
                    </div>
                </div>
                <Show when=move || selected.with(Option::is_some)>
                    <article class="agent-inbox__reader">
                        <div class="agent-inbox__reader-head">
                            <h2 node_ref=reader tabindex="-1">{move || mail.data.with(|value| value.as_ref().map_or("Message".into(), |mail| mail.summary.subject().to_owned()))}</h2>
                            <button type="button" class="k-icon-btn" aria-label="Back to message list" on:click=move |_| {
                                let id = selected.get_untracked();
                                choose(None);
                                request_animation_frame(move || {
                                    let focus = id.and_then(|id| document().get_element_by_id(&format!("inbox-message-{id}")))
                                        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok());
                                    if let Some(focus) = focus { let _ = focus.focus(); }
                                    else if let Some(heading) = heading.get_untracked() { let _ = heading.focus(); }
                                });
                            }>{kit::icon(kit::icons().left)}</button>
                        </div>
                        {kit::error(mail.error)}
                        {move || match mail.data.get() {
                            Some(mail) => view! { <>
                                <dl><dt>"From"</dt><dd>{mail.summary.from}</dd><dt>"To"</dt><dd>{mail.to}</dd><dt>"Received"</dt><dd>{received(mail.summary.received)}</dd></dl>
                                <p class="agent-inbox__body">{if mail.text.is_empty() { "No message text.".into() } else { mail.text }}</p>
                                {mail.text_truncated.then(|| view! { <p class="k-field__hint">"Showing the first 32,000 characters."</p> })}
                            </> }.into_any(),
                            None if mail.error.with(String::is_empty) => view! { <p role="status">"Loading message..."</p> }.into_any(),
                            None => ().into_any(),
                        }}
                    </article>
                </Show>
            </div>
        </div></div></div></main>
    }
}
