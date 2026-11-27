use leptos::{
    __reexports::send_wrapper::SendWrapper, prelude::*, reactive::computed::ScopedFuture,
};
use std::{future::Future, pin::Pin, rc::Rc};

pub(crate) type ChunkView = Result<AnyView, leptos::wasm_split::SplitLoaderError>;

type Render = Rc<dyn Fn() -> Pin<Box<dyn Future<Output = Option<AnyView>>>>>;

/// Renders a lazy view, with a status while it loads and Retry when it fails.
pub(crate) fn load<F, E>(
    label: &'static str,
    render: impl Fn() -> F + Send + Clone + 'static,
) -> impl IntoView
where
    F: Future<Output = Result<AnyView, E>> + 'static,
{
    // Every lazy view shares one copy of the Suspense below.
    let render: Render = Rc::new(move || {
        let view = render();
        Box::pin(async move { view.await.ok() })
    });
    suspense(label, SendWrapper::new(render))
}

fn suspense(label: &'static str, render: SendWrapper<Render>) -> AnyView {
    let attempt = RwSignal::new(0_u32);
    view! {
        <Suspense fallback=move || view! {
            <span class="sr-only" role="status">{format!("Loading {label}…")}</span>
        }>
            {move || {
                attempt.track();
                let view = render();
                Suspend::new(SendWrapper::new(async move {
                    // Reads while the view builds must not rebuild it later.
                    match ScopedFuture::new_untracked(view).await {
                        Some(view) => view,
                        None => view! {
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
    .into_any()
}
