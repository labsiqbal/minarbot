use leptos::prelude::*;
use wasm_bindgen::prelude::*;

pub(crate) fn workflow_scope(path: &str) -> Option<(String, String)> {
    let query = path.strip_prefix("/workflows?")?;
    let params = web_sys::UrlSearchParams::new_with_str(query).ok()?;
    if params
        .get_all("ui")
        .iter()
        .any(|value| value.as_string().as_deref() == Some("react"))
    {
        return None;
    }
    let owner = params.get("agent")?.to_ascii_lowercase();
    if !crate::workspace_route::valid_id(&owner) {
        return None;
    }
    let base = format!("/agents/{owner}");
    let back = params
        .get("from")
        .filter(|path| {
            path == &base
                || path
                    .strip_prefix(&format!("{base}/conversations/"))
                    .is_some_and(crate::workspace_route::valid_id)
        })
        .unwrap_or(base);
    Some((owner, back))
}

#[wasm_bindgen]
extern "C" {
    #[cfg(feature = "conversation")]
    #[wasm_bindgen(js_name = minarbotPreloadWorkflows)]
    pub(crate) fn preload_workflows(owner: &str, include_library: bool);

    #[wasm_bindgen(catch, js_name = minarbotMountRoute)]
    async fn mount_route(
        host: &web_sys::HtmlElement,
        kind: &str,
        first: &str,
        second: &str,
    ) -> Result<js_sys::Function, JsValue>;
}

pub(crate) fn outlet(
    kind: &'static str,
    label: &'static str,
    first: String,
    second: String,
    back: Option<String>,
) -> impl IntoView {
    let host = NodeRef::<leptos::html::Div>::new();
    let mounted = StoredValue::new_local(None::<js_sys::Function>);
    let status = RwSignal::new(None::<bool>);
    let load = Callback::new(move |()| {
        let Some(host) = host.get_untracked() else {
            return;
        };
        status.set(None);
        let (first, second) = (first.clone(), second.clone());
        leptos::task::spawn_local(async move {
            match mount_route(&host, kind, &first, &second).await {
                Ok(dispose) => {
                    if mounted
                        .try_update_value(|slot| *slot = Some(dispose.clone()))
                        .is_none()
                    {
                        let _ = dispose.call0(&JsValue::UNDEFINED);
                    }
                    status.try_set(Some(true));
                }
                Err(_) => {
                    status.try_set(Some(false));
                }
            }
        });
    });
    host.on_load(move |_| load.run(()));
    on_cleanup(move || {
        if let Some(dispose) = mounted.try_update_value(Option::take).flatten() {
            let _ = dispose.call0(&JsValue::UNDEFINED);
        }
    });
    view! {
        <div class="lazy-route-mount" node_ref=host></div>
        <div class="conversation-startup" hidden=move || status.get()==Some(true)>
            <p role="status" hidden=move || status.with(Option::is_some)>{format!("Loading {label}…")}</p>
            <p class="message-error" role="alert" hidden=move || status.get()!=Some(false)>{format!("Could not load {label}.")}</p>
            <button type="button" class="k-btn k-btn--sm" hidden=move || status.get()!=Some(false) on:click=move |_| load.run(())>"Retry"</button>
            {back.map(|href| view! { <a class="k-link" data-scoped-workflow-back="" href=href>"Back"</a> })}
        </div>
    }
}
