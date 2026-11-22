use super::{date_time, kit::*, number, Query};
use leptos::prelude::*;
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, PartialEq, Deserialize)]
struct Call {
    account: Option<String>,
    family: String,
    agent_name: String,
    model: String,
    origin: String,
    purpose: String,
    phase: String,
    status: String,
    created: String,
    input_tokens: Value,
    output_tokens: Value,
    total_tokens: Value,
}
#[derive(Clone, PartialEq, Deserialize)]
struct Report {
    transcription: Value,
    summary: Value,
    calls: Vec<Call>,
    next_before: Option<u64>,
}

fn call_row(call: Call) -> impl IntoView {
    let purpose = match (call.phase.as_str(), call.purpose.as_str()) {
        ("image", _) => "Image generation",
        ("compaction", _) => "History summary",
        (_, "routing") => "Channel recipient selection",
        _ => "Agent or workflow reply",
    };
    let status = match call.status.as_str() {
        "pending" => "Waiting for response",
        "received" => "Response received",
        "failed" => "Failed or canceled",
        "interrupted" => "Disconnected, result unknown",
        _ => "",
    };
    let provider = if call.origin == "local-harness" {
        call.model.clone()
    } else {
        match call.family.as_str() {
            "codex" => "Codex app-server",
            "claude" => "Claude Code",
            "openai-images" => "OpenAI Images-compatible",
            "gemini" => "Gemini GenerateContent",
            "anthropic" => "Anthropic Messages",
            _ => "OpenAI-compatible",
        }
        .into()
    };
    let provider = move || view! { {provider.clone()}<br /><span class="k-field__hint usage-origin" title=call.origin.clone()>{call.origin.clone()}</span> };
    let account = if call.phase == "image" {
        "Separate image provider".into()
    } else {
        call.account.unwrap_or_else(|| "Server default".into())
    };
    let [input, output, total] =
        [call.input_tokens, call.output_tokens, call.total_tokens].map(number);
    view! {
        <tr>
          <td><strong>{call.agent_name}</strong><br /><span class="k-field__hint">
            {call.model}<br />{account}<br />{date_time(call.created, "en-US")}</span>
            <div class="usage-call__compact k-field__hint">{provider()}<details><summary>"Details"</summary>
              <div>{purpose}</div><div>{format!("Input {input} · Output {output} · Total {total}")}</div>
            </details></div>
          </td>
          <td>{provider()}</td><td>{purpose}</td><td>{status}</td>
          <td class="is-num">{input}</td><td class="is-num">{output}</td><td class="is-num">{total}</td>
        </tr>
    }
}

pub fn panel() -> impl IntoView {
    let cursors = RwSignal::new(Vec::<u64>::new());
    let calls = RwSignal::new(true);
    let query = Query::<Report>::new(
        move || {
            cursors.with(|values| {
                values.last().map_or_else(
                    || "/api/usage".into(),
                    |id| format!("/api/usage?before={id}"),
                )
            })
        },
        5000,
        RwSignal::new(false),
    );
    let report = query.value;
    let next = move || report.with(|value| value.as_ref().and_then(|value| value.next_before));
    let page = move |older: bool| {
        if older {
            if let Some(id) = untrack(next) {
                cursors.update(|values| values.push(id));
            }
        } else {
            cursors.update(|values| {
                values.pop();
            });
        }
        crate::settings::focus_heading();
    };
    let totals = [
        ("Calls", "calls"),
        ("Waiting", "pending"),
        ("Failed or canceled", "failed"),
        ("Disconnected", "interrupted"),
        ("Input tokens", "input_tokens"),
        ("Output tokens", "output_tokens"),
        ("Total tokens", "total_tokens"),
    ];
    let transcription = [
        ("Calls", "calls"),
        ("Pending", "pending"),
        ("Uncertain", "uncertain"),
        ("Cancelled", "cancelled"),
        ("Usage unknown", "unknown"),
        ("Input tokens", "input_tokens"),
        ("Output tokens", "output_tokens"),
        ("Total tokens", "total_tokens"),
        ("Duration", "seconds"),
    ];
    view! {
      <div class="k-page bs-root set4-page">
        <Header title="Usage" />
        <Show when=move || !query.error.with(String::is_empty)>
          <Alert tone="error" actions=content(move || view! { <Button label=text("Retry") size="sm" on_click=move || query.retry() /> })>{query.error}</Alert>
        </Show>
        <div class="k-tabs">
          <div class="k-tabs__list" role="tablist" aria-label="Usage"
            on:keydown=move |event: web_sys::KeyboardEvent| crate::settings::move_focus(&event, "[role=tablist]", true)>
            {[(true, "Calls", "usage-calls-tab", "usage-calls-panel"), (false, "Transcription", "usage-transcription-tab", "usage-transcription-panel")].map(|(tab, label, id, controls)| view! {
              <button type="button" role="tab" id=id aria-controls=controls class="k-tabs__tab"
                aria-selected=move || (calls.get() == tab).to_string()
                data-state=move || if calls.get() == tab { "active" } else { "inactive" }
                tabindex=move || if calls.get() == tab { 0 } else { -1 }
                on:click=move |_| calls.set(tab)>{label}</button>
            })}
          </div>
          <Show when=move || report.with(Option::is_some) fallback=move || view! {
            <Show when=move || query.error.with(String::is_empty)>{skeleton(3, "Loading usage...")}</Show>
          }>
            <div role="tabpanel" id="usage-calls-panel" aria-labelledby="usage-calls-tab" class="k-tabs__panel" hidden=move || !calls.get()>
              {hint("Usage details", "Provider-reported counts since tracking began. Totals may be incomplete; failed or disconnected calls may still be billed. Tool responses count per call, not per message.")}
              <div class="k-card set4-totals">
                {totals.map(|(label, field)| view! { <Row title=text(label) trailing=content(move || view! {
                  {move || report.with(|value| value.as_ref().map(|value| number(value.summary[field].clone())))}
                }) /> })}
              </div>
              <Section title="Call history">
                {move || report.with(|value| value.as_ref().map(|value| {
                    if value.calls.is_empty() { return empty("No calls", icons().receipt).into_any(); }
                    view! { <div class="k-table-wrap"><table class="k-table usage-calls">
                      <colgroup><col class="usage-agent-col" /><col class="usage-provider-col" /><col class="usage-purpose-col" /><col class="usage-status-col" /><col class="usage-number-col" span="3" /></colgroup>
                      <thead><tr><th scope="col">"Agent and model"</th><th scope="col">"Provider"</th><th scope="col">"Purpose"</th><th scope="col">"Status"</th><th scope="col" class="is-num">"Input"</th><th scope="col" class="is-num">"Output"</th><th scope="col" class="is-num">"Total"</th></tr></thead>
                      <tbody>{value.calls.iter().cloned().map(call_row).collect_view()}</tbody>
                    </table></div> }.into_any()
                }))}
                <div class="k-pager" hidden=move || cursors.with(Vec::is_empty) && next().is_none()>
                  <button type="button" class="k-icon-btn" aria-label="Newer calls" disabled=move || cursors.with(Vec::is_empty) on:click=move |_| page(false)>{icon(icons().left)}</button>
                  <button type="button" class="k-icon-btn" aria-label="Older calls" disabled=move || next().is_none() on:click=move |_| page(true)>{icon(icons().right)}</button>
                </div>
              </Section>
            </div>
            <div role="tabpanel" id="usage-transcription-panel" aria-labelledby="usage-transcription-tab" class="k-tabs__panel" hidden=move || calls.get()>
              {hint("Usage details", "Provider-reported duration and tokens; missing values remain unknown. Latest attempt details are in the source file.")}
              <div class="k-card set4-totals">
                {transcription.map(|(label, field)| view! { <Row title=text(label) trailing=content(move || view! {
                  {move || report.with(|value| value.as_ref().map(|value| {
                    let raw = value.transcription[field].clone();
                    if field == "seconds" && !raw.is_null() { format!("{} seconds", number(raw)) } else { number(raw) }
                  }))}
                }) /> })}
              </div>
            </div>
          </Show>
        </div>
      </div>
    }
}
