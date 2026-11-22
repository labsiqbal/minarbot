use super::{api, discard, kit::*, restore, save, uuid, valid_uuid, Query};
use leptos::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const KEY: &str = "minarbot:shared-data-drop";
#[derive(Clone, PartialEq, Deserialize)]
struct Table {
    name: String,
    owner: String,
    columns: Vec<Column>,
}
#[derive(Clone, PartialEq, Deserialize)]
struct Column {
    name: String,
    #[serde(rename = "type")]
    kind: String,
}
#[derive(Clone, PartialEq, Deserialize)]
struct Listing {
    tables: Vec<Table>,
    next_after: Option<String>,
    revision: u64,
}
#[derive(Clone, Deserialize, Serialize)]
struct ResultSet {
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
    truncated: bool,
    changes: u64,
    revision: u64,
}
#[derive(Clone, Deserialize, Serialize)]
struct Drop {
    id: String,
    revision: u64,
    table: String,
}

pub fn panel() -> impl IntoView {
    let after = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let sql = RwSignal::new(String::new());
    let result = RwSignal::new(None::<ResultSet>);
    let message = RwSignal::new(String::new());
    let pending = RwSignal::new(
        restore::<Option<Drop>>(KEY)
            .ok()
            .flatten()
            .flatten()
            .filter(|command| {
                valid_uuid(&command.id)
                    && !command.table.is_empty()
                    && command.table.encode_utf16().count() <= 128
                    && command.revision <= 9_007_199_254_740_991
            }),
    );
    let confirm = use_context::<Confirm>();
    let directory = use_context::<ReadSignal<Option<Result<crate::shell::Directory, String>>>>();
    let query = Query::<Listing>::new(
        move || {
            format!(
                "/api/data?after={}",
                String::from(js_sys::encode_uri_component(&after.get()))
            )
        },
        5000,
        busy,
    );
    let execute = move |drop: Option<Drop>| {
        let Some(listing) = query.value.get_untracked() else {
            return;
        };
        if busy.get_untracked()
            || (drop.is_none()
                && (!query.fresh.get_untracked() || pending.with_untracked(Option::is_some)))
        {
            return;
        }
        let body = drop.as_ref().map_or_else(|| {
            uuid().map(|id| json!({"id": id, "revision": listing.revision, "sql": sql.get_untracked()}))
        }, |command| {
            save(KEY, command)?;
            serde_json::to_value(command).map_err(|_| "Delete request could not be created.".into())
        });
        let body = match body {
            Ok(body) => body,
            Err(error) => {
                message.set(error);
                return;
            }
        };
        busy.set(true);
        message.set(String::new());
        let path = if let Some(command) = &drop {
            pending.set(Some(command.clone()));
            "/api/data/drop"
        } else {
            "/api/data/query"
        };
        leptos::task::spawn_local(async move {
            let response = api::post::<ResultSet>(path, &body)
                .await
                .and_then(|response| {
                    if let Some(command) = drop {
                        if restore::<Drop>(KEY)?.is_some_and(|stored| stored.id == command.id) {
                            discard(KEY)?;
                        }
                    }
                    Ok(response)
                });
            match response {
                Ok(value) => {
                    pending.try_set(None);
                    result.try_set(Some(value));
                }
                Err(error) => {
                    message.try_set(error);
                }
            }
            busy.try_set(false);
        });
    };
    let drop_table = move |table: String, revision| {
        ask(
            confirm,
            format!("Delete table {table} and all its data?"),
            "This action cannot be undone.",
            "Delete table",
            true,
            move || {
                if busy.get_untracked()
                    || pending.with_untracked(Option::is_some)
                    || !query.fresh.get_untracked()
                {
                    return;
                }
                match uuid() {
                    Ok(id) => execute(Some(Drop {
                        id,
                        revision,
                        table: table.clone(),
                    })),
                    Err(error) => message.set(error),
                }
            },
        );
    };
    let locked = Signal::derive(move || busy.get() || pending.with(Option::is_some));
    let error_text = move || {
        let local = message.get();
        if local.is_empty() {
            query.error.get()
        } else {
            local
        }
    };
    let next = move || {
        query.value.with(|listing| {
            listing
                .as_ref()
                .and_then(|listing| listing.next_after.clone())
        })
    };
    view! {
      <div class="k-page bs-root set4-page">
        <Header title="Shared data" />
        <Show when=move || !error_text().is_empty()><Alert tone="error">{error_text}</Alert></Show>
        <Show when=move || pending.with(Option::is_some)>
          <Show when=move || busy.get() fallback=move || view! {
            <Alert tone="warn" title=Signal::derive(move || pending.with(|command| command.as_ref().map_or_else(String::new, |command| format!("Not confirmed: delete {}", command.table))))
              actions=content(move || view! {
                <Button label=text("Try again") size="sm" disabled=Signal::derive(move || query.value.with(Option::is_none)) on_click=move || execute(pending.get_untracked()) />
                <Button label=text("Discard request") size="sm" on_click=move || match discard(KEY) {
                    Ok(()) => { pending.set(None); query.retry(); },
                    Err(error) => message.set(error),
                } />
              }) />
          }><span class="k-progress" role="status"><span class="k-spinner" aria-hidden="true"></span>"Applying change..."</span></Show>
        </Show>
        <section class="k-section" aria-labelledby="shared-data-tables">
          <div class="k-section__head"><h2 id="shared-data-tables" class="k-section__title"><span title="Agent-shared tables; schema owned by creator. Do not store secrets or private data here.">"Tables"</span></h2></div>
          {move || match query.value.get() {
              None if !query.error.with(String::is_empty) => empty("Data is unavailable.", String::new()).into_any(),
              None => skeleton(3, "Loading tables...").into_any(),
              Some(listing) if listing.tables.is_empty() => empty("No tables", icons().database_empty).into_any(),
              Some(listing) => {
                let revision = listing.revision;
                view! { <Card>{listing.tables.into_iter().map(|table| {
                  let name = table.name.clone();
                  let owner = StoredValue::new(table.owner);
                  let columns = table.columns.iter().map(|column| format!("{}: {}", column.name, if column.kind.is_empty() { "untyped" } else { &column.kind })).collect::<Vec<_>>().join("\n");
                  view! { <div><Row title=text(table.name.clone()) leading=icons().database
                    meta=content(move || view! { <span>{move || {
                        let id = owner.get_value();
                        directory.and_then(|directory| directory.get()).and_then(Result::ok)
                            .and_then(|directory| directory.agents.into_iter().find(|agent| agent.id == id).map(|agent| agent.name)).unwrap_or(id)
                    }}</span> })
                    trailing=content(move || view! {
                      <Menu name=format!("Menu {name}") disabled=Signal::derive(move || locked.get() || !query.fresh.get())
                        items=vec![MenuItem { label: format!("Delete table {name}"), danger: true, action: Callback::new(move |_| drop_table(name.clone(), revision)) }]>
                        {icon(icons().more)}
                      </Menu>
                    }) />
                    <div class="set4-schema"><Disclosure title="Schema">{code(format!("Columns in {}", table.name), columns)}</Disclosure></div>
                  </div> }
                }).collect_view()}</Card> }.into_any()
              },
          }}
          <div class="k-pager" hidden=move || after.with(String::is_empty) && next().is_none()>
            <button type="button" class="k-icon-btn" aria-label="First page" disabled=move || busy.get() || after.with(String::is_empty)
              on:click=move |_| { after.set(String::new()); crate::settings::focus_heading(); }>{icon(icons().left)}</button>
            <button type="button" class="k-icon-btn" aria-label="Next tables" disabled=move || busy.get() || next().is_none()
              on:click=move |_| if let Some(value) = untrack(next) { after.set(value); crate::settings::focus_heading(); }>{icon(icons().right)}</button>
          </div>
        </section>
        <Show when=move || query.value.with(Option::is_some)>
          <Disclosure title="SQL query">
            <Field id="shared-data-sql" label="SQL query"><textarea id="shared-data-sql" class="k-input k-textarea" rows="4" maxlength="16000"
              placeholder=r#"SELECT * FROM "table_name" LIMIT 50"# aria-description="Read-only SQL. No writes or schema changes."
              prop:value=sql on:input=move |event| sql.set(event_target_value(&event)) disabled=locked /></Field>
            <div class="k-row-actions"><Button label=Signal::derive(move || if busy.get() { "Processing...".to_owned() } else { "Run query".into() }) variant="primary" busy=busy
              on_click=move || execute(None) disabled=Signal::derive(move || locked.get() || !query.fresh.get() || sql.with(|sql| sql.trim().is_empty())) /></div>
          </Disclosure>
        </Show>
        {move || result.get().map(|value| {
          let raw = serde_json::to_string_pretty(&value).unwrap_or_default();
          view! {
            <Disclosure title="Last query results" open=true>
              {if value.columns.is_empty() { empty("No result columns", String::new()).into_any() } else { view! {
                <div class="k-table-wrap"><table class="k-table"><caption class="sr-only">"Last query results"</caption>
                  <thead><tr>{value.columns.iter().map(|column| view! { <th scope="col">{column.clone()}</th> }).collect_view()}</tr></thead>
                  <tbody>{value.rows.iter().map(|row| view! { <tr>{row.iter().map(|cell| view! {
                    <td class="system-query-cell" class:is-num=cell.is_number()>{cell.as_str().map_or_else(|| cell.to_string(), str::to_owned)}</td>
                  }).collect_view()}</tr> }).collect_view()}</tbody>
                </table></div>
              }.into_any()}}
              <p class="k-field__hint" role="status">{format!("{} rows returned. {} changes. Revision {}.{}", value.rows.len(), value.changes, value.revision, if value.truncated { " Results truncated at 200 rows." } else { "" })}</p>
              <Disclosure title="Response details">{code("Query response".into(), raw)}</Disclosure>
            </Disclosure>
          }
        })}
      </div>
    }
}
