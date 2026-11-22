use super::*;
use wasm_bindgen::JsCast;

#[component]
pub(super) fn Select(
    name: &'static str,
    #[prop(optional, into)] label: Option<String>,
    #[prop(optional_no_strip)] described_by: Option<&'static str>,
    value: Signal<String>,
    options: Signal<Vec<(String, String)>>,
    change: Callback<String>,
    #[prop(optional, into)] disabled: Signal<bool>,
    #[prop(optional)] small: bool,
) -> impl IntoView {
    let trigger = NodeRef::<leptos::html::Button>::new();
    let list = NodeRef::<leptos::html::Div>::new();
    let expanded = RwSignal::new(false);
    let selected = move || {
        options.with(|options| {
            options
                .iter()
                .find(|(id, _)| *id == value.get())
                .map(|(_, label)| label.clone())
                .unwrap_or_default()
        })
    };
    let close = move || {
        if let Some(list) = list.get_untracked() {
            let _ = list.hide_popover();
        }
        if let Some(trigger) = trigger.get_untracked() {
            let _ = trigger.focus();
        }
    };
    let open = move |last: bool| {
        let (Some(trigger), Some(list)) = (trigger.get_untracked(), list.get_untracked()) else {
            return;
        };
        if list.matches(":popover-open").unwrap_or(false) {
            close();
            return;
        }
        let rect = trigger.get_bounding_client_rect();
        let viewport = window()
            .inner_height()
            .ok()
            .and_then(|height| height.as_f64())
            .unwrap_or(0.0);
        let width = window()
            .inner_width()
            .ok()
            .and_then(|width| width.as_f64())
            .unwrap_or_default();
        let above = (rect.top() - 14.0).max(0.0);
        let below = (viewport - rect.bottom() - 14.0).max(0.0);
        let target_width = rect.width().max(200.0).min((width - 16.0).max(0.0));
        let style = web_sys::HtmlElement::style(&list);
        let _ = style.set_property("width", &format!("{target_width}px"));
        let _ = style.set_property("max-height", &format!("{}px", above.max(below).min(320.0)));
        if list.show_popover().is_err() {
            return;
        }
        let height = f64::from(list.offset_height());
        let left = rect.left().min(width - target_width - 8.0).max(8.0);
        let top = if height <= below || below >= above {
            rect.bottom() + 6.0
        } else {
            (rect.top() - height - 6.0).max(8.0)
        };
        let _ = style.set_property("left", &format!("{left}px"));
        let _ = style.set_property("top", &format!("{top}px"));
        let focus = list
            .query_selector("button[aria-selected=true]")
            .ok()
            .flatten()
            .or_else(|| {
                list.query_selector(if last { "button:last-child" } else { "button" })
                    .ok()
                    .flatten()
            });
        if let Some(option) = focus.and_then(|item| item.dyn_into::<web_sys::HtmlElement>().ok()) {
            let _ = option.focus();
        }
    };
    let list_id = format!("{name}-list");
    let controls = list_id.clone();
    let label_for = label.clone();
    let labelled_by = if label.is_some() { None } else { Some(name) };
    let typeahead = RwSignal::new((0.0_f64, String::new()));
    view! {
        <button id=name node_ref=trigger type="button" role="combobox" class="k-input k-select" class:k-select--sm=small
            aria-label=label aria-describedby=described_by aria-controls=controls aria-haspopup="listbox" aria-expanded=move || expanded.get().to_string()
            data-state=move || if expanded.get() {"open"}else{"closed"} disabled=disabled
            on:click=move |_| open(false)
            on:keydown=move |event: web_sys::KeyboardEvent| {
                if matches!(event.key().as_str(),"ArrowDown"|"ArrowUp") {
                    event.prevent_default();open(event.key()=="ArrowUp");
                }
            }><span>{selected}</span><span class="k-select__chev"><ui::Icon name="chevron-down" /></span></button>
        <div id=list_id node_ref=list popover="auto" role="listbox" class="k-pop k-select__content system-menu automation-select bs-root"
            aria-label=label_for aria-labelledby=labelled_by
            on:toggle=move |_| expanded.set(list.get_untracked().is_some_and(|list| list.matches(":popover-open").unwrap_or(false)))
            on:keydown=move |event: web_sys::KeyboardEvent| {
                match event.key().as_str() {
                    "Escape" => {event.prevent_default();event.stop_propagation();close();}
                    "Tab" => close(),
                    "ArrowUp"|"ArrowDown"|"Home"|"End" => crate::settings::move_focus(&event,"[role=listbox]",false),
                    key if key.chars().count()==1 && key!=" " && !event.ctrl_key() && !event.meta_key() && !event.alt_key() => {
                        let Some(list)=list.get_untracked() else {return;};
                        let Ok(items)=list.query_selector_all("button") else {return;};
                        let now=js_sys::Date::now();
                        let query=typeahead.with_untracked(|(at,prefix)| {
                            if now-*at>500.0 {key.to_lowercase()} else {format!("{prefix}{}",key.to_lowercase())}
                        });
                        typeahead.set((now,query.clone()));
                        let buttons=(0..items.length()).filter_map(|index|items.item(index)?.dyn_into::<web_sys::HtmlElement>().ok()).collect::<Vec<_>>();
                        let index=document().active_element().and_then(|active| buttons.iter().position(|button| active.is_same_node(Some(button)))).unwrap_or(0);
                        let start=if query.chars().count()==1 {index+1}else{index};
                        if let Some(button)=buttons.iter().cycle().skip(start).take(buttons.len()).find(|button| {
                            button.text_content().is_some_and(|label|label.to_lowercase().starts_with(&query))
                        }) {let _=button.focus();}
                    }
                    _=>{},
                }
            }>
            {move || options.get().into_iter().map(|(id,label)| {
                let current=id.clone();
                view! { <button class="k-menu__item" type="button" tabindex="-1" role="option"
                    aria-selected=move || (value.get()==current).to_string()
                    on:click=move |_| {close();change.run(id.clone());}>{label}</button> }
            }).collect_view()}
        </div>
    }
}
