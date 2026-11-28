use super::{api, kit::*, poll, Query};
use leptos::prelude::*;
use serde::Deserialize;
use serde_json::json;

const PATH: &str = "/api/settings/computer";

#[derive(Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    state: State,
    runtime: Option<String>,
    missing_programs: Vec<String>,
    can_setup: bool,
    message: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum State {
    NoRuntime,
    ImageMissing,
    Downloading,
    Ready,
    Failed,
}

impl Status {
    fn label(&self) -> String {
        match self.state {
            State::NoRuntime => "No container runtime".into(),
            State::ImageMissing => "Image missing".into(),
            State::Downloading if self.runtime.is_none() => "Setting up Computer".into(),
            State::Downloading => "Downloading image".into(),
            State::Ready => self
                .runtime
                .as_ref()
                .map_or_else(|| "Ready".into(), |name| format!("Ready · {name}")),
            State::Failed => "Setup failed".into(),
        }
    }
}

pub fn panel() -> impl IntoView {
    let busy = RwSignal::new(false);
    let status = Query::<Status>::new(|| PATH.into(), 5000, busy);
    let command_error = RwSignal::new(String::new());
    let downloading = move || {
        status.value.with(|status| {
            status
                .as_ref()
                .is_some_and(|status| status.state == State::Downloading)
        })
    };
    poll(1000, move || {
        if untrack(downloading) {
            status.retry();
        }
    });
    let setup = move || {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        command_error.set(String::new());
        leptos::task::spawn_local(async move {
            match api::post::<Status>(PATH, &json!({})).await {
                Ok(next) => {
                    status.value.try_set(Some(next));
                }
                Err(message) => {
                    command_error.try_set(message);
                }
            }
            busy.try_set(false);
        });
    };
    let status_error = move || {
        let error = command_error.get();
        if !error.is_empty() {
            return error;
        }
        if !status.error.with(String::is_empty) {
            return "Computer status could not load.".into();
        }
        status.value.with(|status| {
            status
                .as_ref()
                .and_then(|status| status.message.clone())
                .unwrap_or_default()
        })
    };
    view! {
      <div class="k-page bs-root">
        <Header title="Computer" />
        <div class="k-stack">
          <Show when=move || status.value.with(Option::is_some) fallback=move || view! {
            <Show when=move || status.error.with(String::is_empty)>{skeleton(1, "Loading Computer...")}</Show>
          }>
            <Card>
              <Row title=text("This device") leading=icons().monitor
                meta=content(move || view! {
                  <span role="status">{move || status.value.with(|value| value.as_ref().map(Status::label))}</span>
                  <span class="k-row__meta" hidden=move || status.value.with(|value| value.as_ref().is_none_or(|status| status.missing_programs.is_empty()))>
                    {move || status.value.with(|value| value.as_ref().map(|status| format!("Missing programs - {}", status.missing_programs.join(", "))))}
                  </span>
                })
                trailing=content(move || view! {
                  <Show when=move || status.value.with(|value| value.as_ref().is_some_and(|status| status.can_setup))>
                    <Button label=text("Set up Computer") variant="primary" on_click=setup busy=Signal::derive(move || busy.get() || downloading()) />
                  </Show>
                }) />
            </Card>
          </Show>
          <Show when=move || !status_error().is_empty()>
            <Alert tone="error">{status_error}</Alert>
          </Show>
        </div>
      </div>
    }
}
