use leptos::prelude::*;
use wasm_bindgen::JsCast;

#[derive(Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Icons {
    pub alert: String,
    pub info: String,
    pub warn: String,
    pub close: String,
    pub monitor: String,
    pub monitor_empty: String,
    pub download: String,
    pub smartphone: String,
    pub database: String,
    pub database_empty: String,
    pub key: String,
    pub key_empty: String,
    pub receipt: String,
    pub plus: String,
    pub down: String,
    pub left: String,
    pub right: String,
    pub arrow: String,
    pub copy: String,
    pub check: String,
    pub more: String,
    pub refresh: String,
}

pub fn icons() -> Icons {
    use_context::<Icons>().unwrap_or_default()
}

pub fn icon(svg: String) -> impl IntoView {
    view! { <span class="appearance-icon" aria-hidden="true" inner_html=svg></span> }
}

#[component]
pub fn Header(title: &'static str, #[prop(optional)] actions: Option<Children>) -> impl IntoView {
    view! { <header class="k-page-head"><div class="k-page-head__text"><h1 tabindex="-1">{title}</h1></div>
    {actions.map(|actions| view! { <div class="k-page-head__actions">{actions()}</div> })}</header> }
}

#[component]
pub fn Section(
    title: &'static str,
    children: Children,
    #[prop(optional)] actions: Option<Children>,
) -> impl IntoView {
    view! { <section class="k-section"><div class="k-section__head"><h2 class="k-section__title">{title}</h2>{actions.map(|actions| view! { <div class="k-section__actions">{actions()}</div> })}</div>{children()}</section> }
}

#[component]
pub fn Row(
    #[prop(into)] title: Signal<String>,
    #[prop(optional)] leading: String,
    #[prop(optional)] meta: Option<Children>,
    #[prop(optional)] trailing: Option<Children>,
    #[prop(optional)] action: Option<Callback<()>>,
) -> impl IntoView {
    let chevron = action.is_some() && trailing.is_none();
    let body = view! {
        {(!leading.is_empty()).then(|| view! { <span class="k-row__leading">{icon(leading)}</span> })}
        <span class="k-row__text"><span class="k-row__title">{title}</span>
            {meta.map(|meta| view! { <span class="k-row__meta">{meta()}</span> })}
        </span>
        {chevron.then(|| icon(icons().right))}
    };
    view! {
        <div class="k-row" class:is-clickable=action.is_some()>
            {if let Some(action) = action {
                view! { <button type="button" class="k-row__main" on:click=move |_| action.run(())>{body}</button> }.into_any()
            } else {
                view! { <div class="k-row__main">{body}</div> }.into_any()
            }}
            {trailing.map(|trailing| view! { <span class="k-row__trailing">{trailing()}</span> })}
        </div>
    }
}

pub fn badge(tone: &str, label: impl IntoView) -> impl IntoView {
    view! { <span class=format!("k-badge is-{tone}")>{label}</span> }
}

#[component]
pub fn Button(
    #[prop(into)] label: Signal<String>,
    on_click: impl Fn() + 'static,
    #[prop(default = "secondary")] variant: &'static str,
    #[prop(default = "md")] size: &'static str,
    #[prop(default = Signal::derive(|| false), into)] disabled: Signal<bool>,
    #[prop(default = Signal::derive(|| false), into)] busy: Signal<bool>,
    #[prop(optional)] name: Option<String>,
) -> impl IntoView {
    view! { <button type="button" class=format!("k-btn k-btn--{variant} k-btn--{size}") class:is-busy=busy
        disabled=move || disabled.get() || busy.get() aria-busy=move || busy.get().then_some("true")
        aria-label=name on:click=move |_| on_click()>
        <span class="k-spinner" aria-hidden="true" hidden=move || !busy.get()></span>{label}
    </button> }
}

#[component]
pub fn Alert(
    #[prop(default = "info")] tone: &'static str,
    #[prop(optional, into)] title: Option<Signal<String>>,
    #[prop(optional)] children: Option<Children>,
    #[prop(optional)] actions: Option<Children>,
) -> impl IntoView {
    let icons = icons();
    let icon_svg = match tone {
        "error" => icons.alert,
        "warn" => icons.warn,
        _ => icons.info,
    };
    view! {
        <div class=format!("k-alert is-{tone}") role=if tone == "error" { "alert" } else { "status" }>
            <span class="appearance-icon k-alert__icon" aria-hidden="true" inner_html=icon_svg></span>
            <div class="k-alert__body">
                {title.map(|title| view! { <div class="k-alert__title">{title}</div> })}
                {children.map(|content| view! { <div class="k-alert__text">{content()}</div> })}
                {actions.map(|actions| view! { <div class="k-alert__actions">{actions()}</div> })}
            </div>
        </div>
    }
}

pub fn error(message: RwSignal<String>) -> impl IntoView {
    view! { <div hidden=move || message.with(String::is_empty)><Alert tone="error">{message}</Alert></div> }
}

pub fn empty(title: &'static str, svg: String) -> impl IntoView {
    view! { <div class="k-empty">{(!svg.is_empty()).then(|| view! { <span class="k-empty__icon" aria-hidden="true">{icon(svg)}</span> })}
        <div class="k-empty__title">{title}</div>
    </div> }
}

pub fn skeleton(count: u32, label: &'static str) -> impl IntoView {
    let shown = RwSignal::new(false);
    let timer = set_timeout_with_handle(
        move || {
            shown.try_set(true);
        },
        std::time::Duration::from_millis(150),
    );
    if let Ok(timer) = timer {
        on_cleanup(move || timer.clear());
    }
    view! { <span class="sr-only" role="status">{label}</span>
    <div class="k-card" aria-hidden="true" hidden=move || !shown.get()>
      {(0..count).map(|index| view! { <div class="k-row"><div class="k-row__main"><span class="k-row__text">
          <span class="k-skeleton" style:width=format!("{}%", 46 + index * 17 % 30)></span>
          <span class="k-skeleton is-small" style:width=format!("{}%", 22 + index * 11 % 20)></span>
      </span></div></div> }).collect_view()}
    </div> }
}

#[component]
pub fn Field(id: &'static str, label: &'static str, children: Children) -> impl IntoView {
    view! { <div class="k-field"><label class="k-field__label" for=id>{label}</label>{children()}</div> }
}

#[component]
pub fn Disclosure(
    #[prop(into)] title: String,
    #[prop(optional)] open: bool,
    children: Children,
) -> impl IntoView {
    let chevron = icons().down;
    let expanded = RwSignal::new(open);
    view! { <details class="k-collapsible system-disclosure" data-state=move || if expanded.get() { "open" } else { "closed" } open=open on:toggle=move |event| {
      expanded.set(event.target().and_then(|node| node.dyn_into::<web_sys::Element>().ok()).is_some_and(|node| node.has_attribute("open")));
    }><summary class="k-collapsible__trigger" role="button" aria-expanded=move || expanded.get().to_string()>
      <span class="k-collapsible__title">{title}</span>
      <span class="k-collapsible__chev">{icon(chevron)}</span>
      </summary><div class="k-collapsible__content">{children()}</div></details> }
}

pub fn hint(text: &'static str, description: &'static str) -> impl IntoView {
    view! { <span class="system-tooltip k-field__hint" tabindex="0" aria-description=description>
        {text}<span class="k-tooltip set4-tooltip" aria-hidden="true">{description}</span>
    </span> }
}

#[component]
pub fn Dialog(
    id: &'static str,
    #[prop(into)] title: Signal<String>,
    #[prop(into)] open: Signal<bool>,
    on_close: impl Fn() + 'static,
    #[prop(default = "md")] size: &'static str,
    #[prop(default = Signal::derive(String::new), into)] description: Signal<String>,
    #[prop(optional)] footer: Option<Children>,
    #[prop(optional)] actions: Option<Children>,
    #[prop(optional)] children: Option<Children>,
    #[prop(optional)] confirmation: bool,
    #[prop(optional)] hide_header: bool,
    #[prop(optional)] select_input: bool,
    #[prop(default = Signal::derive(|| false), into)] locked: Signal<bool>,
) -> impl IntoView {
    let node = NodeRef::<leptos::html::Dialog>::new();
    let on_close = StoredValue::new_local(on_close);
    on_cleanup(move || {
        if let Some(dialog) = node.try_get().flatten() {
            dialog.close();
        }
    });
    Effect::new(move |_| {
        if let Some(dialog) = node.get() {
            if open.get() && !dialog.open() {
                let connected = dialog.is_connected();
                let show = move || {
                    if open.try_get_untracked() != Some(true)
                        || !dialog.is_connected()
                        || dialog.open()
                        || dialog.show_modal().is_err()
                        || confirmation
                    {
                        return;
                    }
                    let field = dialog.query_selector(".k-dialog__body :is(input:not([type=hidden]):not([type=checkbox]), textarea):not([readonly]):not(:disabled), .k-dialog__body [autofocus]").ok().flatten();
                    if let Some(field) =
                        field.and_then(|field| field.dyn_into::<web_sys::HtmlElement>().ok())
                    {
                        let _ = field.focus();
                        if select_input {
                            if let Ok(input) = field.dyn_into::<web_sys::HtmlInputElement>() {
                                request_animation_frame(move || {
                                    if document().active_element().as_ref() == Some(input.as_ref())
                                    {
                                        input.select();
                                    }
                                });
                            }
                        }
                    } else {
                        let _ = dialog.focus();
                    }
                };
                if connected {
                    show();
                } else {
                    // Lazy views can build the dialog before inserting it into the document.
                    request_animation_frame(show);
                }
            } else if !open.get() && dialog.open() {
                dialog.close();
            }
        }
    });
    let heading_id = format!("{id}-title");
    let desc_id = format!("{id}-description");
    let title_for = heading_id.clone();
    let desc_for = desc_id.clone();
    let backdrop_press = RwSignal::new(false);
    view! {
      <dialog node_ref=node class=format!("k-dialog bs-root k-dialog--{size} system-dialog") tabindex="-1"
        role=confirmation.then_some("alertdialog")
        aria-labelledby=title_for
        aria-describedby=move || (!description.with(String::is_empty)).then(|| desc_for.clone())
        on:cancel=move |event: web_sys::Event| {
            event.prevent_default();
            event.stop_propagation();
            if !locked.get_untracked() { on_close.with_value(|close| close()); }
        }
        on:pointerdown=move |event| backdrop_press.set(event.target() == event.current_target())
        on:click=move |event| {
            if !confirmation && !locked.get_untracked() && backdrop_press.get_untracked() && event.target() == event.current_target() {
                on_close.with_value(|close| close());
            }
        }>
        <div class=if hide_header { "sr-only" } else { "k-dialog__head" }><div>
          <h2 id=heading_id class="k-dialog__title">{title}</h2>
          <p id=desc_id class="k-dialog__desc" hidden=move || description.with(String::is_empty)>{description}</p>
        </div>
        {actions.map(|actions| actions())}
        {(!confirmation && !hide_header).then(|| view! {
          <button type="button" class="k-icon-btn" aria-label="Close" disabled=locked on:click=move |_| on_close.with_value(|close| close())>{icon(icons().close)}</button>
        })}</div>
        {children.map(|children| view! { <div class="k-dialog__body">{children()}</div> })}
        {footer.map(|footer| view! { <div class="k-dialog__foot">{footer()}</div> })}
      </dialog>
    }
}

pub type Confirm = RwSignal<Option<ConfirmRequest>>;

#[derive(Clone)]
pub struct ConfirmRequest {
    title: String,
    description: String,
    label: &'static str,
    danger: bool,
    action: Callback<()>,
}

pub fn ask(
    confirm: Option<Confirm>,
    title: impl Into<String>,
    description: impl Into<String>,
    label: &'static str,
    danger: bool,
    action: impl Fn() + Send + Sync + 'static,
) {
    if let Some(confirm) = confirm {
        confirm.set(Some(ConfirmRequest {
            title: title.into(),
            description: description.into(),
            label,
            danger,
            action: Callback::new(move |()| action()),
        }));
    }
}

pub fn confirmation() -> (Confirm, impl IntoView) {
    named_confirmation("system-confirm")
}

pub fn named_confirmation(id: &'static str) -> (Confirm, impl IntoView) {
    let request: Confirm = RwSignal::new(None);
    let cancel = move || request.set(None);
    let view = view! {
        <Dialog id=id title=Signal::derive(move || request.with(|value| value.as_ref().map(|request| request.title.clone()).unwrap_or_default()))
          description=Signal::derive(move || request.with(|value| value.as_ref().map(|request| request.description.clone()).unwrap_or_default()))
          open=Signal::derive(move || request.with(Option::is_some)) on_close=cancel size="sm" confirmation=true
          footer=content(move || view! {
            <button type="button" class="k-btn k-btn--secondary k-btn--md" autofocus on:click=move |_| cancel()>"Cancel"</button>
            <button type="button" class=move || if request.with(|value| value.as_ref().is_some_and(|request| request.danger)) { "k-btn k-btn--danger k-btn--md" } else { "k-btn k-btn--primary k-btn--md" }
              on:click=move |_| { if let Some(value) = request.get_untracked() { cancel(); value.action.run(()); } }>
              {move || request.with(|value| value.as_ref().map_or("Continue", |request| request.label))}
            </button>
          })>
        </Dialog>
    };
    (request, view)
}

#[component]
pub fn Card(children: Children) -> impl IntoView {
    view! { <div class="k-card">{children()}</div> }
}

pub fn text(value: impl Into<String>) -> Signal<String> {
    Signal::stored(value.into())
}

pub fn code(label: String, value: String) -> impl IntoView {
    view! { <pre class="k-code system-code" tabindex="0" role="region" aria-label=label>{value}</pre> }
}

pub struct MenuItem {
    pub label: String,
    pub danger: bool,
    pub action: Callback<()>,
}

#[component]
pub fn Menu(
    name: String,
    children: Children,
    items: Vec<MenuItem>,
    #[prop(optional)] item_icons: Vec<&'static str>,
    #[prop(optional)] separate_danger: bool,
    #[prop(default = "k-icon-btn")] class: &'static str,
    #[prop(default = Signal::derive(|| false), into)] disabled: Signal<bool>,
) -> impl IntoView {
    let trigger = NodeRef::<leptos::html::Button>::new();
    let menu = NodeRef::<leptos::html::Div>::new();
    let expanded = RwSignal::new(false);
    let menu_label = name.clone();
    let show = move |last: bool| {
        let (Some(trigger), Some(menu)) = (trigger.get_untracked(), menu.get_untracked()) else {
            return;
        };
        if menu.matches(":popover-open").unwrap_or(false) {
            let _ = menu.hide_popover();
            return;
        }
        if menu.show_popover().is_err() {
            return;
        }
        let rect = trigger.get_bounding_client_rect();
        let width = f64::from(menu.offset_width());
        let height = f64::from(menu.offset_height());
        let viewport = window()
            .inner_height()
            .ok()
            .and_then(|value| value.as_f64())
            .unwrap_or(0.0);
        let left = (rect.right() - width).max(8.0);
        let top = if rect.bottom() + height + 6.0 <= viewport {
            rect.bottom() + 6.0
        } else {
            (rect.top() - height - 6.0).max(8.0)
        };
        let _ = web_sys::HtmlElement::style(&menu).set_property("left", &format!("{left}px"));
        let _ = web_sys::HtmlElement::style(&menu).set_property("top", &format!("{top}px"));
        if let Ok(Some(button)) =
            menu.query_selector(if last { "button:last-child" } else { "button" })
        {
            if let Ok(button) = button.dyn_into::<web_sys::HtmlElement>() {
                let _ = button.focus();
            }
        }
    };
    view! {
        <button node_ref=trigger type="button" class=class aria-label=name
          data-state=move || if expanded.get() { "open" } else { "closed" }
          aria-haspopup="menu" aria-expanded=move || if expanded.get() { "true" } else { "false" } disabled=disabled
          on:click=move |_| show(false) on:keydown=move |event: web_sys::KeyboardEvent| {
            if matches!(event.key().as_str(), "ArrowDown" | "ArrowUp") {
                event.prevent_default();
                show(event.key() == "ArrowUp");
            }
          }>{children()}</button>
        <div node_ref=menu popover="auto" class="k-pop k-menu bs-root system-menu" role="menu" aria-label=menu_label
          on:toggle=move |_| expanded.set(menu.get_untracked().is_some_and(|menu| menu.matches(":popover-open").unwrap_or(false)))
          on:keydown=move |event: web_sys::KeyboardEvent| {
            if event.key() == "Escape" {
                event.prevent_default();
                event.stop_propagation();
                if let Some(menu) = menu.get_untracked() { let _ = menu.hide_popover(); }
                if let Some(trigger) = trigger.get_untracked() { let _ = trigger.focus(); }
            } else {
                crate::settings::move_focus(&event, ".system-menu", false);
                if let Some(menu) = menu.get_untracked().filter(|_| event.key().chars().count() == 1 && event.key() != " ") {
                    if let Ok(items) = menu.query_selector_all("button") {
                        let key = event.key().to_lowercase();
                        let buttons = (0..items.length()).filter_map(|index| items.item(index)?.dyn_into::<web_sys::HtmlElement>().ok()).collect::<Vec<_>>();
                        let after = document().active_element().and_then(|active| buttons.iter().position(|button| active.is_same_node(Some(button)))).map_or(0, |index| index + 1);
                        if let Some(button) = buttons.iter().cycle().skip(after).take(buttons.len()).find(|button| button.text_content().is_some_and(|text| text.trim_start().to_lowercase().starts_with(&key))) {
                            let _ = button.focus();
                        }
                    }
                }
            }
          }>
          {items.into_iter().enumerate().map(|(index, item)| view! {
            {(separate_danger && item.danger).then(|| view! { <div class="k-menu__sep" role="separator"></div> })}
            <button class="k-menu__item" class:is-danger=item.danger type="button" role="menuitem" tabindex="-1"
            on:click=move |_| {
                if let Some(menu) = menu.get_untracked() { let _ = menu.hide_popover(); }
                if let Some(trigger) = trigger.get_untracked() { let _ = trigger.focus(); }
                item.action.run(());
            }>{item_icons.get(index).map(|name| view! { <crate::connection_ui::Icon name=*name /> })}{item.label}</button> }).collect_view()}
        </div>
    }
}

pub fn content<V: IntoView>(render: impl FnOnce() -> V + Send + 'static) -> Children {
    Box::new(move || render().into_any())
}
