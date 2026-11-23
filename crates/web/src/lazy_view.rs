use leptos::{__reexports::send_wrapper::SendWrapper, prelude::*};
use std::future::Future;

pub(crate) type ChunkView = Result<AnyView, leptos::wasm_split::SplitLoaderError>;

pub(crate) fn load<F>(
    label: &'static str,
    render: impl Fn() -> F + Send + Clone + 'static,
) -> impl IntoView
where
    F: Future<Output = ChunkView> + 'static,
{
    let attempt = RwSignal::new(0_u32);
    view! {
        <Suspense fallback=move || view! {
            <span class="sr-only" role="status">{format!("Loading {label}…")}</span>
        }>
            {move || {
                attempt.track();
                let render = render.clone();
                Suspend::new(SendWrapper::new(async move {
                    match render().await {
                        Ok(view) => view,
                        Err(_) => view! {
                            <div class="conversation-startup">
                                <p class="message-error" role="alert">{format!("Could not load {label}.")}</p>
                                <button type="button" class="k-btn k-btn--sm"
                                    on:click=move |_| attempt.update(|value| *value += 1)>"Retry"</button>
                            </div>
                        }.into_any(),
                    }
                }))
            }}
        </Suspense>
    }
}
