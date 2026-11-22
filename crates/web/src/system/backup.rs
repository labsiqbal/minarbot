use super::{api, kit::*, uuid};
use gloo_net::http::Request;
use leptos::prelude::*;
use serde_json::json;
use wasm_bindgen::JsCast;

async fn download(signal: &web_sys::AbortSignal) -> Result<(), String> {
    let response = Request::post("/api/backup")
        .abort_signal(Some(signal))
        .json(&json!({"id": uuid()?, "confirmed": true}))
        .map_err(|_| "Backup request could not be created.")?
        .send()
        .await
        .map_err(|_| "Connection lost. Try downloading the backup again.")?;
    if !response.ok() {
        return Err(api::response_error(response, "Backup could not be created. Try again.").await);
    }
    let response = web_sys::Response::from(response);
    let blob = response
        .blob()
        .map_err(|_| "Backup download failed.")?
        .await
        .map_err(|_| "Backup download failed.")?
        .dyn_into::<web_sys::Blob>()
        .map_err(|_| "Backup download failed.")?;
    if signal.aborted() {
        return Ok(());
    }
    let anchor = document()
        .create_element("a")
        .map_err(|_| "Backup download failed.")?
        .dyn_into::<web_sys::HtmlAnchorElement>()
        .map_err(|_| "Backup download failed.")?;
    let url =
        web_sys::Url::create_object_url_with_blob(&blob).map_err(|_| "Backup download failed.")?;
    anchor.set_href(&url);
    anchor.set_download("minarbot-workspace.sqlite");
    anchor.click();
    set_timeout(
        move || {
            let _ = web_sys::Url::revoke_object_url(&url);
        },
        std::time::Duration::from_secs(1),
    );
    Ok(())
}

pub fn panel() -> impl IntoView {
    let restore = RwSignal::new(false);
    let confirm = use_context::<Confirm>();
    let busy = RwSignal::new(false);
    let message = RwSignal::new(String::new());
    let downloaded = RwSignal::new(false);
    let request = StoredValue::new_local(None::<web_sys::AbortController>);
    on_cleanup(move || {
        request.with_value(|request| {
            if let Some(request) = request {
                request.abort();
            }
        })
    });
    let start = move || {
        ask(confirm, "Download workspace backup?", "Backup includes all conversations, files, instructions, and webhook secrets. The file is not encrypted. Keep it private.", "Download backup", false, move || {
            if busy.get_untracked() { return; }
            let Ok(controller) = web_sys::AbortController::new() else { message.set("Backup download failed.".into()); return; };
            let signal = controller.signal();
            request.set_value(Some(controller));
            busy.set(true); downloaded.set(false); message.set(String::new());
            leptos::task::spawn_local(async move {
                let result = download(&signal).await;
                if signal.aborted() { return; }
                request.set_value(None);
                busy.try_set(false);
                match result {
                    Ok(()) => { downloaded.try_set(true); },
                    Err(error) => { message.try_set(error); },
                }
            });
        });
    };
    view! {
      <div class="k-page bs-root set4-page">
        <Header title="Backup & restore" />
        {error(message)}
        <span class="k-field__hint" role="status" hidden=move || !downloaded.get()>"Download started. Check the saved file."</span>
        <Section title="Backup"><Card><Row title=text("Workspace backup") trailing=content(move || view! {
          <Button label=text("Download") variant="primary" busy=busy on_click=start />
        }) /></Card></Section>
        <Section title="Restore"><Card><Row title=text("Restore backup") trailing=content(move || view! {
          {badge("neutral", "Command line")}<Button label=text("Open") on_click=move || restore.set(true) />
        }) /></Card></Section>
        <Dialog id="backup-restore" title=text("Restore backup") open=restore size="lg" on_close=move || restore.set(false)
          footer=content(move || view! { <Button label=text("Close") on_click=move || restore.set(false) /> })>
          <div class="k-stack set4-detail">
            <p>"Restore only trusted backups. Recovery uses a new database and does not overwrite the old workspace. Schedules, webhooks, and script access are disabled; unfinished work is held for review."</p>
            <p>"Run from the application directory on the server computer:"</p>
            {code("Backup restore command".into(), "./bin/minarbot-server restore workspace BACKUP.sqlite --db /new-directory/minarbot.sqlite".into())}
            <p>"The destination directory must already exist. To use the restored data, point MINARBOT_DATA_DIR to that directory when starting the application. Check the results and settings before enabling automation again."</p>
            <p class="k-field__hint">"Backup includes conversations, files, instructions, routines, workflows, results and webhook secrets. Server-environment provider settings and browser drafts are excluded. Restore does not configure deployment or credentials."</p>
          </div>
        </Dialog>
      </div>
    }
}
