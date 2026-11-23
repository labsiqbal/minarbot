use crate::api;
use leptos::{html, prelude::*, task::spawn_local};
use serde::de::DeserializeOwned;
use wasm_bindgen::JsCast;
use web_sys::{Event, HtmlElement, HtmlFormElement, KeyboardEvent, SubmitEvent};

pub type UiAction = Callback<()>;

pub struct Query<T: Send + Sync + 'static> {
    pub data: RwSignal<Option<T>>,
    pub error: RwSignal<String>,
    pub busy: RwSignal<bool>,
    pub reload: UiAction,
    generation: RwSignal<u64>,
}

impl<T: Send + Sync + 'static> Copy for Query<T> {}
impl<T: Send + Sync + 'static> Clone for Query<T> {
    fn clone(&self) -> Self {
        *self
    }
}

pub fn query<T: DeserializeOwned + PartialEq + Send + Sync + 'static>(
    path: impl Fn() -> String + Send + Sync + 'static,
) -> Query<T> {
    query_with_prefetch(path, None)
}

pub(crate) fn query_with_prefetch<T: DeserializeOwned + PartialEq + Send + Sync + 'static>(
    path: impl Fn() -> String + Send + Sync + 'static,
    initial: Option<js_sys::Promise>,
) -> Query<T> {
    let data = RwSignal::new(None);
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(true);
    let generation = RwSignal::new(0_u64);
    let path = StoredValue::new(path);
    let initial = StoredValue::new_local(initial);
    let reload = UiAction::new(move |()| {
        let path = path.with_value(|path| path());
        generation.update(|n| *n += 1);
        let current = generation.get_untracked();
        error.set(String::new());
        if path.is_empty() {
            data.set(None);
            busy.set(false);
            return;
        }
        busy.set(true);
        let prefetched = initial.try_update_value(Option::take).flatten();
        spawn_local(async move {
            let result = match prefetched {
                Some(response) => api::read_prefetched(response).await,
                None => api::read(&path).await,
            };
            if generation.try_get_untracked() != Some(current) {
                return;
            }
            match result {
                Ok(result) => data.maybe_update(|current| {
                    if current.as_ref() == Some(&result) {
                        false
                    } else {
                        *current = Some(result);
                        true
                    }
                }),
                Err(message) => error.set(message),
            }
            busy.set(false);
        });
    });
    reload.run(());
    Query {
        data,
        error,
        busy,
        reload,
        generation,
    }
}

// Keep only the initial page for warm project tab navigation. Always revalidate it.
pub(crate) fn cached_query<T: Clone + DeserializeOwned + PartialEq + Send + Sync + 'static>(
    path: impl Fn() -> String + Copy + Send + Sync + 'static,
    cache: Option<StoredValue<Option<T>>>,
) -> Query<T> {
    let state = query(path);
    if let Some(cache) = cache {
        let initial_path = path();
        state.data.set(cache.get_value());
        Effect::new(move |_| {
            if !state.busy.get() && state.error.with(String::is_empty) && path() == initial_path {
                cache.set_value(state.data.get_untracked());
            }
        });
    }
    state
}

impl<T: Send + Sync + 'static> Query<T> {
    pub fn poll(self, milliseconds: u64, enabled: impl Fn() -> bool + 'static) {
        poll_with_backoff(
            milliseconds,
            move || {
                (enabled() && !self.busy.get_untracked())
                    .then(|| !self.error.with_untracked(String::is_empty))
            },
            move || self.reload.run(()),
        );
    }

    pub fn invalidate(self) {
        self.generation.update(|generation| *generation += 1);
        self.busy.set(false);
    }
}

pub fn uuid() -> String {
    window()
        .crypto()
        .map(|crypto| crypto.random_uuid())
        .unwrap_or_default()
}

pub fn encode(value: &str) -> String {
    js_sys::encode_uri_component(value)
        .as_string()
        .unwrap_or_default()
}

pub fn form(event: &SubmitEvent) -> Option<web_sys::FormData> {
    event.prevent_default();
    let element = event.target()?.dyn_into::<HtmlFormElement>().ok()?;
    web_sys::FormData::new_with_form(&element).ok()
}

pub fn field(data: &web_sys::FormData, name: &str) -> String {
    data.get(name).as_string().unwrap_or_default()
}

pub async fn copy(text: &str) -> Result<(), wasm_bindgen::JsValue> {
    wasm_bindgen_futures::JsFuture::from(window().navigator().clipboard().write_text(text)).await?;
    Ok(())
}

pub fn local_agents() -> Vec<crate::shell::Agent> {
    use_context::<ReadSignal<Option<Result<crate::shell::Directory, String>>>>()
        .and_then(|directory| directory.get())
        .and_then(Result::ok)
        .map_or_else(Vec::new, |directory| directory.agents)
}

#[component]
pub fn Icon(name: &'static str) -> impl IntoView {
    let icon = with_context::<Icons, _>(|icons| icons.0.get(name).cloned()).flatten();
    view! { <span class="appearance-icon" aria-hidden="true" inner_html=icon.unwrap_or_default()></span> }
}

#[derive(Clone, Default, serde::Deserialize)]
#[serde(transparent)]
pub struct Icons(pub std::collections::BTreeMap<String, String>);

#[component]
pub fn Header(
    #[prop(into)] title: Signal<String>,
    #[prop(optional)] actions: Option<Children>,
) -> impl IntoView {
    view! {
        <header class="k-page-head">
            <div class="k-page-head__text"><h1 tabindex="-1">{title}</h1></div>
            {actions.map(|actions| view! { <div class="k-page-head__actions">{actions()}</div> })}
        </header>
    }
}

#[component]
pub fn Section(
    #[prop(into)] title: String,
    #[prop(optional)] actions: Option<Children>,
    children: Children,
) -> impl IntoView {
    view! {
        <section class="k-section"><div class="k-section__head">
            <h2 class="k-section__title">{title}</h2>
            {actions.map(|actions| view! { <div class="k-section__actions">{actions()}</div> })}
        </div>{children()}</section>
    }
}

#[component]
pub fn Row(
    #[prop(into)] title: String,
    #[prop(optional)] icon: &'static str,
    #[prop(optional)] meta: Option<Children>,
    #[prop(optional)] trailing: Option<Children>,
    #[prop(optional)] click: Option<UiAction>,
    #[prop(optional, into)] disabled: Signal<bool>,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    let chevron = click.is_some() && trailing.is_none();
    let content = view! {
        {(!icon.is_empty()).then(|| view! { <span class="k-row__leading"><Icon name=icon /></span> })}
        <span class="k-row__text">
            <span class="k-row__title">{title}</span>
            {meta.map(|meta| view! { <span class="k-row__meta">{meta()}</span> })}
            {children.map(|children| children())}
        </span>
        {chevron.then(|| view! { <span class="k-row__chev"><Icon name="chevron-right" /></span> })}
    };
    let main = match click {
        Some(click) => view! { <button type="button" class="k-row__main" disabled=disabled on:click=move |_| click.run(())>{content}</button> }.into_any(),
        None => view! { <div class="k-row__main">{content}</div> }.into_any(),
    };
    view! {
        <div class="k-row" class:is-clickable=click.is_some()>
            {main}{trailing.map(|trailing| view! { <span class="k-row__trailing">{trailing()}</span> })}
        </div>
    }
}

#[component]
pub fn Button(
    #[prop(into)] label: String,
    #[prop(optional, into)] aria: Option<String>,
    #[prop(default = "secondary")] variant: &'static str,
    #[prop(default = "md")] size: &'static str,
    #[prop(optional)] icon: &'static str,
    #[prop(optional, into)] disabled: Signal<bool>,
    #[prop(optional, into)] busy: Signal<bool>,
    #[prop(optional)] click: Option<UiAction>,
    #[prop(optional)] submit: bool,
    #[prop(optional)] form: Option<&'static str>,
    #[prop(optional)] value: Option<&'static str>,
) -> impl IntoView {
    view! {
        <button type=if submit { "submit" } else { "button" } class=format!("k-btn k-btn--{variant} k-btn--{size}") class:is-busy=busy
            disabled=move || disabled.get() || busy.get() aria-busy=move || busy.get().then_some("true")
            aria-label=aria form=form value=value
            on:click=move |_| { if let Some(click) = click { click.run(()); } }>
            {move || if busy.get() {
                view! { <span class="k-spinner" aria-hidden="true"></span> }.into_any()
            } else if !icon.is_empty() {
                view! { <Icon name=icon /> }.into_any()
            } else { ().into_any() }}
            {label}
        </button>
    }
}

#[component]
pub fn Badge(children: Children, #[prop(optional)] success: bool) -> impl IntoView {
    view! { <span class=if success { "k-badge is-success" } else { "k-badge is-neutral" }>{children()}</span> }
}

#[component]
pub fn Empty(
    #[prop(into)] title: String,
    #[prop(optional)] icon: &'static str,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    view! {
        <div class="k-empty">
            {(!icon.is_empty()).then(|| view! { <span class="k-empty__icon"><Icon name=icon /></span> })}
            <div class="k-empty__title">{title}</div>{children.map(|children| children())}
        </div>
    }
}

#[component]
pub fn Loading(
    #[prop(default = 3)] count: usize,
    #[prop(default = "Loading…")] label: &'static str,
) -> impl IntoView {
    view! {
        <span class="sr-only" role="status">{label}</span>
        <div class="k-card" aria-hidden="true">
            {(0..count).map(|index| view! {
                <div class="k-row"><div class="k-row__main"><span class="k-row__text">
                    <span class="k-skeleton" style:width=format!("{}%", 46 + (index * 17) % 30)></span>
                    <span class="k-skeleton is-small" style:width=format!("{}%", 22 + (index * 11) % 20)></span>
                </span></div></div>
            }).collect_view()}
        </div>
    }
}

#[component]
pub fn Alert(
    #[prop(default = "error")] tone: &'static str,
    #[prop(optional)] actions: Option<Children>,
    children: Children,
) -> impl IntoView {
    view! {
        <div class=format!("k-alert is-{tone}") role=if tone == "error" { "alert" } else { "status" }>
            <Icon name=match tone { "error" => "error", "warn" => "warn", _ => "info" } />
            <div class="k-alert__body"><div class="k-alert__text">{children()}</div>
                {actions.map(|actions| view! { <div class="k-alert__actions">{actions()}</div> })}
            </div>
        </div>
    }
}

#[component]
pub fn ErrorNotice(
    error: Signal<String>,
    #[prop(optional)] retry: Option<UiAction>,
    #[prop(default = "Retry")] label: &'static str,
) -> impl IntoView {
    view! { <Show when=move || !error.get().is_empty()>
        <Alert>{error}{retry.map(|retry| view! {
            <div class="k-alert__actions"><Button label=label size="sm" click=retry /></div>
        })}</Alert>
    </Show> }
}

#[component]
pub fn Field(
    #[prop(into)] label: String,
    name: &'static str,
    #[prop(optional, into)] hint: Option<String>,
    #[prop(optional)] optional: bool,
    children: Children,
) -> impl IntoView {
    view! {
        <div class="k-field">
            <label class="k-field__label" for=name>
                {label}{optional.then(|| view! { <span class="k-field__optional">"Optional"</span> })}
            </label>
            {children()}
            {hint.filter(|hint| !hint.is_empty()).map(|hint| view! { <span class="k-field__hint" id=format!("{name}-hint")>{hint}</span> })}
        </div>
    }
}

#[component]
pub fn Select(
    name: &'static str,
    #[prop(optional, into)] label: Option<String>,
    value: Signal<String>,
    options: Signal<Vec<(String, String)>>,
    change: Callback<String>,
    #[prop(optional, into)] disabled: Signal<bool>,
    #[prop(optional)] small: bool,
) -> impl IntoView {
    view! {
        <span class="connection-select" class:is-small=small>
            <select class="k-input k-select" class:k-select--sm=small id=name name=name aria-label=label
                disabled=disabled
                on:change=move |event| change.run(event_target_value(&event))>
                {move || options.get().into_iter().map(|(key, label)| {
                    let current = key.clone();
                    view! { <option value=key prop:selected=move || value.get() == current>{label}</option> }
                }).collect_view()}
            </select>
            <span class="k-select__chev"><Icon name="chevron-down" /></span>
        </span>
    }
}

#[component]
pub fn Switch(
    #[prop(into)] label: String,
    #[prop(into)] checked: Signal<bool>,
    #[prop(optional, into)] disabled: Signal<bool>,
    change: Callback<bool>,
) -> impl IntoView {
    let state = move || {
        if checked.get() {
            "checked"
        } else {
            "unchecked"
        }
    };
    view! {
        <button type="button" role="switch" class="k-switch" aria-label=label
            aria-checked=move || checked.get().to_string()
            data-state=state
            disabled=disabled on:click=move |_| change.run(!checked.get_untracked())>
            <span class="k-switch__thumb" data-state=state></span>
        </button>
    }
}

#[component]
pub fn Check(
    #[prop(into)] label: String,
    #[prop(into)] checked: Signal<bool>,
    #[prop(optional, into)] disabled: Signal<bool>,
    change: Callback<bool>,
) -> impl IntoView {
    view! {
        <label class="k-check" class:is-disabled=disabled>
            <input type="checkbox" class="connection-checkbox k-check__box" prop:checked=checked disabled=disabled
                on:change=move |event| change.run(event_target_checked(&event)) />
            <span class="k-check__text"><span class="k-check__label">{label}</span></span>
        </label>
    }
}

#[component]
pub fn Disclosure(
    #[prop(into)] title: String,
    #[prop(optional)] count: Option<usize>,
    #[prop(optional)] open: bool,
    children: Children,
) -> impl IntoView {
    view! {
        <details class="k-collapsible connection-disclosure" open=open>
            <summary class="k-collapsible__trigger">
                <span class="k-collapsible__title">{title}{count.map(|count| view! { <Badge>{count}</Badge> })}</span>
                <span class="k-collapsible__chev"><Icon name="chevron-down" /></span>
            </summary>
            <div class="k-collapsible__content" data-state="open">{children()}</div>
        </details>
    }
}

#[component]
pub fn Code(#[prop(into)] label: String, #[prop(into)] text: String) -> impl IntoView {
    view! { <pre class="k-code" role="region" aria-label=label tabindex="0"><code>{text}</code></pre> }
}

pub struct MenuItem {
    pub label: String,
    pub action: UiAction,
    pub danger: bool,
    pub disabled: Signal<bool>,
}
impl MenuItem {
    pub fn new(label: impl Into<String>, action: UiAction) -> Self {
        Self {
            label: label.into(),
            action,
            danger: false,
            disabled: false.into(),
        }
    }
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
}

#[component]
pub fn Menu(
    #[prop(into)] label: String,
    items: Vec<MenuItem>,
    #[prop(optional, into)] disabled: Signal<bool>,
    #[prop(optional)] text: bool,
) -> impl IntoView {
    let trigger = NodeRef::<html::Button>::new();
    let menu = NodeRef::<html::Div>::new();
    let id = format!("connection-menu-{}", uuid());
    let open = RwSignal::new(false);
    let focus_row = move |last| {
        if let Some(items) = menu
            .get_untracked()
            .and_then(|menu| menu.query_selector_all("button:not(:disabled)").ok())
        {
            let index = if last {
                items.length().saturating_sub(1)
            } else {
                0
            };
            if let Some(item) = items
                .item(index)
                .and_then(|node| node.dyn_into::<HtmlElement>().ok())
            {
                let _ = item.focus();
            }
        }
    };
    let show = move |last| {
        if let (Some(menu), Some(trigger)) = (menu.get_untracked(), trigger.get_untracked()) {
            let _ = menu.show_popover();
            let anchor = trigger.get_bounding_client_rect();
            let width = f64::from(menu.offset_width());
            let height = f64::from(menu.offset_height());
            let viewport = window()
                .inner_height()
                .ok()
                .and_then(|height| height.as_f64())
                .unwrap_or_default();
            let y = if anchor.bottom() + height + 6.0 <= viewport - 4.0 {
                anchor.bottom() + 6.0
            } else {
                (anchor.top() - height - 6.0).max(4.0)
            };
            let css = web_sys::HtmlElement::style(&menu);
            let _ = css.set_property("left", &format!("{}px", (anchor.right() - width).max(4.0)));
            let _ = css.set_property("top", &format!("{y}px"));
            focus_row(last);
        }
    };
    let items = items.into_iter().map(|item| view! {
        <button type="button" role="menuitem" class="k-menu__item" class:is-danger=item.danger
            tabindex="-1" disabled=item.disabled
            on:click=move |_| {
                if let Some(menu) = menu.get_untracked() { let _ = menu.hide_popover(); }
                if let Some(trigger) = trigger.get_untracked() { let _ = trigger.focus(); }
                item.action.run(());
            }>{item.label}</button>
    }).collect_view();
    view! {
        <button type="button" class=if text { "k-btn k-btn--secondary k-btn--md connection-menu-trigger" } else { "k-icon-btn" } node_ref=trigger aria-label=label.clone() aria-haspopup="menu"
            aria-controls=id.clone() aria-expanded=move || open.get().to_string()
            popovertarget=id.clone() disabled=disabled
            on:click=move |event| { event.prevent_default(); if open.get_untracked() {
                if let Some(menu) = menu.get_untracked() { let _ = menu.hide_popover(); }
            } else { show(false); } }
            on:keydown=move |event: KeyboardEvent| {
                if matches!(event.key().as_str(), "ArrowDown" | "ArrowUp") {
                    event.prevent_default(); show(event.key() == "ArrowUp");
                }
            }>{if text {
                view! { {label.clone()}<Icon name="chevron-down" /> }.into_any()
            } else { view! { <Icon name="ellipsis" /> }.into_any() }}</button>
        <div class="k-pop k-menu connection-menu" popover="auto" role="menu" aria-label=label id=id node_ref=menu
            on:toggle=move |_| open.set(menu.get_untracked().is_some_and(|el| el.matches(":popover-open").unwrap_or(false)))
            on:keydown=move |event: KeyboardEvent| {
                if event.key() == "Escape" {
                    event.prevent_default();
                    event.stop_propagation();
                    if let Some(menu) = menu.get_untracked() { let _ = menu.hide_popover(); }
                    if let Some(trigger) = trigger.get_untracked() { let _ = trigger.focus(); }
                } else if event.key() == "Tab" {
                    if let Some(menu) = menu.get_untracked() { let _ = menu.hide_popover(); }
                    if let Some(trigger) = trigger.get_untracked() { let _ = trigger.focus(); }
                } else {
                    crate::settings::move_focus(&event, "[role=menu]", false);
                    let key = event.key();
                    if key.chars().count() == 1 && key != " " && !event.ctrl_key() && !event.alt_key() && !event.meta_key() {
                        let key = key.to_lowercase();
                        if let Some(items) = menu.get_untracked().and_then(|menu| menu.query_selector_all("button:not(:disabled)").ok()) {
                            let active = document().active_element();
                            let items = (0..items.length()).filter_map(|index| items.item(index)?.dyn_into::<HtmlElement>().ok()).collect::<Vec<_>>();
                            let start = items.iter().position(|item| active.as_ref().is_some_and(|active| item.is_same_node(Some(active)))).map_or(0, |index| index + 1);
                            if let Some(item) = items.iter().cycle().skip(start).take(items.len()).find(|item|
                                item.text_content().unwrap_or_default().trim().to_lowercase().starts_with(&key)) {
                                event.prevent_default();
                                let _ = item.focus();
                            }
                        }
                    }
                }
            }>{items}</div>
    }
}

#[component]
pub fn Modal(
    title: String,
    #[prop(optional, into)] description: Option<String>,
    #[prop(default = "md")] size: &'static str,
    #[prop(optional)] confirm: bool,
    #[prop(optional, into)] busy: Signal<bool>,
    close: UiAction,
    #[prop(optional)] footer: Option<Children>,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    let node = NodeRef::<html::Dialog>::new();
    let title_id = format!("connection-dialog-{}", uuid());
    let description_id = format!("{title_id}-desc");
    let labelled_by = title_id.clone();
    let description = description.filter(|description| !description.is_empty());
    let described_by = description.as_ref().map(|_| description_id.clone());
    let dismiss = move || {
        if !busy.get_untracked() {
            close.run(());
        }
    };
    on_cleanup(move || {
        if let Some(dialog) = node.try_get().flatten() {
            dialog.close();
        }
    });
    Effect::new(move |_| {
        if let Some(dialog) = node.get() {
            if dialog.open() {
                return;
            }
            let connected = dialog.is_connected();
            let selector = if confirm {
                ".k-dialog__foot button"
            } else {
                ".k-dialog__body :is(input:not([type=hidden]):not([type=number]), textarea):not([readonly]):not(:disabled), .k-dialog__body [role=tab][data-state=active]"
            };
            let show = move || {
                if !dialog.is_connected() || dialog.open() || dialog.show_modal().is_err() {
                    return;
                }
                if let Some(first) = dialog
                    .query_selector(selector)
                    .ok()
                    .flatten()
                    .and_then(|node| node.dyn_into::<HtmlElement>().ok())
                {
                    let _ = first.focus();
                } else {
                    let _ = dialog.focus();
                }
            };
            if connected {
                show();
            } else {
                request_animation_frame(show);
            }
        }
    });
    view! {
        <dialog class=format!("k-dialog k-dialog--{size} bs-root connection-dialog") node_ref=node
            tabindex="-1" role=if confirm { "alertdialog" } else { "dialog" }
            aria-labelledby=labelled_by aria-describedby=described_by
            on:cancel=move |event: Event| { event.prevent_default(); event.stop_propagation(); dismiss(); }>
            <div class="k-dialog__head">
                <div><h2 class="k-dialog__title" id=title_id.clone()>{title}</h2>
                    {description.clone().map(|description| view! { <p class="k-dialog__desc" id=description_id.clone()>{description}</p> })}
                </div>
                {(!confirm).then(|| view! {
                    <button type="button" class="k-icon-btn" aria-label="Close" disabled=busy
                        on:click=move |_| dismiss()><Icon name="close" /></button>
                })}
            </div>
            {children.map(|children| view! { <div class="k-dialog__body">{children()}</div> })}
            {footer.map(|footer| view! { <div class="k-dialog__foot">{footer()}</div> })}
        </dialog>
    }
}

#[derive(Clone)]
pub struct Confirmation {
    title: String,
    description: String,
    label: &'static str,
    danger: bool,
    action: UiAction,
}
pub type Confirm = RwSignal<Option<Confirmation>>;

pub fn confirm(
    state: Confirm,
    title: impl Into<String>,
    description: impl Into<String>,
    label: &'static str,
    danger: bool,
    action: UiAction,
) {
    state.set(Some(Confirmation {
        title: title.into(),
        description: description.into(),
        label,
        danger,
        action,
    }));
}

#[component]
pub fn ConfirmDialog(state: Confirm) -> impl IntoView {
    let cancel = UiAction::new(move |()| state.set(None));
    move || {
        state.get().map(|request| {
        let accept = UiAction::new(move |()| { cancel.run(()); request.action.run(()); });
        view! {
            <Modal title=request.title.clone() description=request.description size="sm" confirm=true close=cancel footer=Box::new(move || view! {
                    <Button label="Cancel" click=cancel />
                    <Button label=request.label variant=if request.danger { "danger" } else { "primary" } click=accept />
                }.into_any()) />
        }
    })
    }
}

pub fn redact(message: String, secret: &str) -> String {
    if secret.is_empty() {
        message
    } else {
        message.replace(secret, "[redacted]")
    }
}

pub fn poll(seconds: u64, tick: impl Fn() + 'static) {
    poll_with_backoff(seconds * 1000, || Some(false), tick);
}

type PollActions = (Box<dyn Fn() -> Option<bool>>, Box<dyn Fn()>);

#[derive(Clone, Copy)]
struct Poll {
    interval: u64,
    failures: StoredValue<u32>,
    timer: StoredValue<Option<TimeoutHandle>>,
    retry_at: StoredValue<Option<f64>>,
    actions: StoredValue<PollActions, LocalStorage>,
}

pub(crate) fn backoff(interval: u64, failures: u32) -> u64 {
    interval
        .saturating_mul(1 << failures.min(5))
        .min(interval.max(60_000))
}

impl Poll {
    fn clear(self) {
        if let Some(Some(timer)) = self.timer.try_update_value(Option::take) {
            timer.clear();
        }
    }

    fn schedule(self, delay: u64, retry: bool) {
        self.clear();
        self.retry_at
            .set_value(retry.then(|| js_sys::Date::now() + delay as f64));
        if !document().hidden() {
            self.timer.set_value(
                set_timeout_with_handle(
                    move || self.run(retry),
                    std::time::Duration::from_millis(delay),
                )
                .ok(),
            );
        }
    }

    fn run(self, retry: bool) {
        if document().hidden() {
            return;
        }
        if let Some(failed) = self.actions.with_value(|(status, _)| status()) {
            if !failed {
                self.failures.set_value(0);
            } else if !retry {
                let count = (self.failures.get_value() + 1).min(5);
                self.failures.set_value(count);
                self.schedule(backoff(self.interval, count) - self.interval, true);
                return;
            }
            self.actions.with_value(|(_, tick)| tick());
        }
        self.schedule(self.interval, false);
    }

    fn resume(self) {
        self.clear();
        if !document().hidden() {
            if let Some(when) = self.retry_at.get_value() {
                self.schedule((when - js_sys::Date::now()).max(0.0) as u64, true);
            } else {
                self.run(false);
            }
        }
    }
}

/// A ready source reports whether its last read failed. A busy or inactive
/// source returns None and keeps its generation and request ownership.
pub fn poll_with_backoff(
    milliseconds: u64,
    status: impl Fn() -> Option<bool> + 'static,
    tick: impl Fn() + 'static,
) {
    let poll = Poll {
        interval: milliseconds,
        failures: StoredValue::new(0),
        timer: StoredValue::new(None),
        retry_at: StoredValue::new(None),
        actions: StoredValue::new_local((Box::new(status), Box::new(tick))),
    };
    poll.schedule(milliseconds, false);
    on_visibility(move || poll.resume());
    let focus = window_event_listener(leptos::ev::focus, move |_| poll.resume());
    on_cleanup(move || {
        poll.clear();
        focus.remove();
    });
}

pub(crate) fn on_visibility(changed: impl FnMut() + 'static) {
    let changed = wasm_bindgen::closure::Closure::<dyn FnMut()>::new(changed);
    let _ = document()
        .add_event_listener_with_callback("visibilitychange", changed.as_ref().unchecked_ref());
    let changed = StoredValue::new_local(changed);
    on_cleanup(move || {
        changed.with_value(|changed| {
            let _ = document().remove_event_listener_with_callback(
                "visibilitychange",
                changed.as_ref().unchecked_ref(),
            );
        });
    });
}

pub fn external_url(value: &str) -> Option<String> {
    let url = web_sys::Url::new(value).ok()?;
    (matches!(url.protocol().as_str(), "https:" | "http:")
        && url.username().is_empty()
        && url.password().is_empty())
    .then(|| url.href())
}

pub fn host(url: &str) -> String {
    web_sys::Url::new(url).map_or_else(|_| url.to_owned(), |url| url.host())
}

pub fn agent_options(hide_hidden: bool) -> Vec<(String, String)> {
    local_agents()
        .into_iter()
        .filter(|agent| !hide_hidden || !agent.hidden)
        .map(|agent| (agent.id, agent.name))
        .collect()
}

pub fn scoped_agents(
    hide_hidden: bool,
    members: Option<StoredValue<Vec<String>>>,
) -> Signal<Vec<(String, String)>> {
    let directory = use_context::<ReadSignal<Option<Result<crate::shell::Directory, String>>>>();
    Signal::derive(move || {
        directory
            .and_then(|directory| directory.get())
            .and_then(Result::ok)
            .map_or_else(Vec::new, |directory| {
                directory
                    .agents
                    .into_iter()
                    .filter(|agent| {
                        (!hide_hidden || !agent.hidden)
                            && members.is_none_or(|members| {
                                members.with_value(|ids| ids.contains(&agent.id))
                            })
                    })
                    .map(|agent| (agent.id, agent.name))
                    .collect()
            })
    })
}

pub fn selected_agent(options: Signal<Vec<(String, String)>>) -> RwSignal<String> {
    let agent = RwSignal::new(String::new());
    Effect::new(move |_| {
        let rows = options.get();
        if agent.with_untracked(|id| !rows.iter().any(|(candidate, _)| candidate == id)) {
            agent.set(
                rows.into_iter()
                    .next()
                    .map_or_else(String::new, |(id, _)| id),
            );
        }
    });
    agent
}

pub fn agent_selection(hide_hidden: bool) -> RwSignal<String> {
    let agent = RwSignal::new(String::new());
    let directory = use_context::<ReadSignal<Option<Result<crate::shell::Directory, String>>>>();
    Effect::new(move |_| {
        let Some(Ok(rows)) = directory.and_then(|directory| directory.get()) else {
            return;
        };
        let mut agents = rows
            .agents
            .into_iter()
            .filter(|row| !hide_hidden || !row.hidden);
        if let Some(first) = agents.next() {
            if agent.with_untracked(|id| id != &first.id && !agents.any(|row| &row.id == id)) {
                agent.set(first.id);
            }
        } else {
            agent.set(String::new());
        }
    });
    agent
}

pub type Cursor = RwSignal<Vec<String>>;

pub fn cursor_query(cursor: Cursor) -> String {
    cursor.with_untracked(|rows| {
        rows.last()
            .filter(|row| !row.is_empty())
            .map_or_else(String::new, |row| format!("?after={}", encode(row)))
    })
}

#[component]
pub fn Pager(
    cursor: Cursor,
    next: Signal<Option<String>>,
    previous_label: &'static str,
    next_label: &'static str,
    refresh: UiAction,
    #[prop(optional, into)] disabled: Signal<bool>,
) -> impl IntoView {
    let has_previous = move || !cursor.with(Vec::is_empty);
    view! {
        <Show when=move || has_previous() || next.with(Option::is_some)>
            <div class="k-pager">
                <button type="button" class="k-icon-btn" aria-label=previous_label disabled=move || disabled.get() || !has_previous()
                    on:click=move |_| { cursor.update(|rows| { rows.pop(); }); refresh.run(()); }><Icon name="chevron-left" /></button>
                <button type="button" class="k-icon-btn" aria-label=next_label disabled=move || disabled.get() || next.with(Option::is_none)
                    on:click=move |_| { if let Some(next) = next.get_untracked() { cursor.update(|rows| rows.push(next)); refresh.run(()); } }><Icon name="chevron-right" /></button>
            </div>
        </Show>
    }
}
