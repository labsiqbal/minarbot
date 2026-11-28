//! Resize handles for the workspace sidebar and the conversation details
//! panel on wide screens (768 px and up; phones keep their own layout).
//!
//! Each pane keeps a preferred width per browser in local storage. The
//! applied width is the preference bounded so the chat column keeps
//! [`MAIN`] pixels; it is written to `--sidebar-width` or `--details-width`
//! on the root element, which the wide-screen CSS reads. A pane the owner
//! never resized has no property, so it keeps its stylesheet width.

use leptos::prelude::*;
use wasm_bindgen::JsCast;

/// The chat column keeps at least this width beside both panes.
const MAIN: f64 = 420.0;
const STEP: f64 = 16.0;
const BIG_STEP: f64 = 64.0;
const RESIZING: &str = "is-panel-resizing";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pane {
    /// The workspace sidebar; its handle sits on its right edge.
    Sidebar,
    /// The conversation details panel; its handle sits on its left edge.
    Details,
}

struct Spec {
    min: f64,
    max: f64,
    /// The stylesheet width: 300 px for the sidebar, 360 px for the panel.
    default: f64,
    key: &'static str,
    property: &'static str,
    label: &'static str,
    /// The pane while it takes room beside the chat.
    shown: &'static str,
}

impl Pane {
    const fn index(self) -> usize {
        match self {
            Self::Sidebar => 0,
            Self::Details => 1,
        }
    }

    const fn spec(self) -> Spec {
        match self {
            Self::Sidebar => Spec {
                min: 220.0,
                max: 480.0,
                default: 300.0,
                key: "minarbot:sidebar-width",
                property: "--sidebar-width",
                label: "Resize sidebar",
                shown: ".leptos-workspace > .sidebar:not([hidden])",
            },
            Self::Details => Spec {
                min: 280.0,
                max: 600.0,
                default: 360.0,
                key: "minarbot:details-width",
                property: "--details-width",
                label: "Resize details panel",
                shown: ".conversation-panel-shell.is-resizable[open]:not(.is-overlay)",
            },
        }
    }

    /// Moving the handle right widens the sidebar and narrows the panel.
    const fn grows_right(self) -> bool {
        matches!(self, Self::Sidebar)
    }
}

#[derive(Clone, Copy, Default, PartialEq)]
struct Applied {
    width: f64,
    max: f64,
}

/// Preferred and applied widths of both panes, provided by the shell.
#[derive(Clone, Copy)]
pub(crate) struct Widths {
    preferred: StoredValue<[Option<f64>; 2]>,
    applied: RwSignal<[Applied; 2]>,
}

impl Widths {
    /// Read the stored widths and apply them before the shell first paints.
    pub(crate) fn new() -> Self {
        let read = |pane: Pane| {
            let spec = pane.spec();
            stored(spec.key)
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|width| width.is_finite() && (spec.min..=spec.max).contains(width))
        };
        let widths = Self {
            preferred: StoredValue::new([read(Pane::Sidebar), read(Pane::Details)]),
            applied: RwSignal::new([Applied::default(); 2]),
        };
        widths.fit();
        widths
    }

    /// Bound both preferences to the window and to each other, then write
    /// the CSS properties. The panel gives way before the sidebar.
    pub(crate) fn fit(self) {
        let room = document()
            .document_element()
            .map_or(0.0, |root| f64::from(root.client_width()))
            - MAIN;
        let preferred = self.preferred.get_value();
        let want = |pane: Pane| preferred[pane.index()].unwrap_or(pane.spec().default);
        let limit = |pane: Pane, other: f64| {
            let spec = pane.spec();
            (room - other).min(spec.max).max(spec.min)
        };
        let bound = |pane: Pane, max: f64| Applied {
            width: want(pane).min(max).max(pane.spec().min).round(),
            max: max.round(),
        };
        // The room a shown pane takes from the other one.
        let taken = |pane: Pane, applied: Applied| if shown(pane) { applied.width } else { 0.0 };
        let sidebar_alone = bound(Pane::Sidebar, limit(Pane::Sidebar, 0.0));
        let details = bound(
            Pane::Details,
            limit(Pane::Details, taken(Pane::Sidebar, sidebar_alone)),
        );
        let sidebar = bound(
            Pane::Sidebar,
            limit(Pane::Sidebar, taken(Pane::Details, details)),
        );
        let applied = [sidebar, details];
        if let Some(style) = root_style() {
            for pane in [Pane::Sidebar, Pane::Details] {
                let property = pane.spec().property;
                let _ = if preferred[pane.index()].is_some() {
                    style.set_property(property, &format!("{}px", applied[pane.index()].width))
                } else {
                    style.remove_property(property).map(|_| ())
                };
            }
        }
        if self.applied.get_untracked() != applied {
            self.applied.set(applied);
        }
    }

    fn set(self, pane: Pane, width: f64) {
        let spec = pane.spec();
        let max = self.applied.get_untracked()[pane.index()].max;
        let width = width.min(max).max(spec.min).round();
        self.preferred
            .update_value(|preferred| preferred[pane.index()] = Some(width));
        self.fit();
    }

    fn save(self, pane: Pane) {
        let key = pane.spec().key;
        if let Ok(Some(storage)) = window().local_storage() {
            let _ = match self.preferred.get_value()[pane.index()] {
                Some(width) => storage.set_item(key, &width.to_string()),
                None => storage.remove_item(key),
            };
        }
    }

    /// Forget the preference: the pane returns to its stylesheet width.
    fn reset(self, pane: Pane) {
        self.preferred
            .update_value(|preferred| preferred[pane.index()] = None);
        self.fit();
        self.save(pane);
    }
}

fn stored(key: &str) -> Option<String> {
    window()
        .local_storage()
        .ok()
        .flatten()?
        .get_item(key)
        .ok()?
}

fn root_style() -> Option<web_sys::CssStyleDeclaration> {
    document()
        .document_element()?
        .dyn_into::<web_sys::HtmlElement>()
        .ok()
        .map(|root| root.style())
}

fn shown(pane: Pane) -> bool {
    document()
        .query_selector(pane.spec().shown)
        .ok()
        .flatten()
        .is_some()
}

fn resizing(on: bool) {
    if let Some(root) = document().document_element() {
        let classes = root.class_list();
        let _ = if on {
            classes.add_1(RESIZING)
        } else {
            classes.remove_1(RESIZING)
        };
    }
}

/// The rendered width of the pane that holds `handle`.
fn rendered(handle: &web_sys::Element) -> Option<f64> {
    Some(handle.parent_element()?.get_bounding_client_rect().width())
}

/// A `separator` on the pane's inner edge: drag it, use the arrow keys
/// (Shift for bigger steps, Home and End for the bounds), or double-click
/// it to return to the default width. Renders nothing without a shell.
pub(crate) fn handle(pane: Pane) -> Option<impl IntoView> {
    let widths = use_context::<Widths>()?;
    let spec = pane.spec();
    let drag = StoredValue::new(None::<(f64, f64)>);
    let dragging = RwSignal::new(false);
    let end = move || {
        if drag.get_value().is_some() {
            drag.set_value(None);
            dragging.set(false);
            resizing(false);
            widths.save(pane);
        }
    };
    on_cleanup(move || {
        if drag.try_get_value().flatten().is_some() {
            resizing(false);
        }
    });
    let handle_of = |event: &web_sys::Event| {
        event
            .current_target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
    };
    let applied = move || widths.applied.get()[pane.index()];
    Some(view! {
        <div class="panel-resize-handle" class:panel-resize-handle--right=pane.grows_right()
            class:panel-resize-handle--left=!pane.grows_right()
            role="separator" aria-orientation="vertical" tabindex="0" aria-label=spec.label
            aria-valuemin=format!("{}", spec.min) aria-valuemax=move || format!("{}", applied().max)
            aria-valuenow=move || format!("{}", applied().width)
            data-dragging=move || dragging.get().then_some("")
            on:pointerdown=move |event: web_sys::PointerEvent| {
                if event.button() != 0 { return; }
                let Some(handle) = handle_of(&event) else { return; };
                let Some(start) = rendered(&handle) else { return; };
                let _ = handle.set_pointer_capture(event.pointer_id());
                widths.fit();
                drag.set_value(Some((f64::from(event.client_x()), start)));
                dragging.set(true);
                resizing(true);
            }
            on:pointermove=move |event: web_sys::PointerEvent| {
                if let Some((x, start)) = drag.get_value() {
                    let delta = f64::from(event.client_x()) - x;
                    widths.set(pane, if pane.grows_right() { start + delta } else { start - delta });
                }
            }
            on:pointerup=move |_| end()
            on:pointercancel=move |_| end()
            on:lostpointercapture=move |_| end()
            on:dblclick=move |_| widths.reset(pane)
            on:keydown=move |event: web_sys::KeyboardEvent| {
                let Some(current) = handle_of(&event).as_ref().and_then(rendered) else { return; };
                let step = if event.shift_key() { BIG_STEP } else { STEP };
                let outward = if pane.grows_right() { step } else { -step };
                widths.fit();
                let width = match event.key().as_str() {
                    "ArrowRight" => current + outward,
                    "ArrowLeft" => current - outward,
                    "Home" => spec.min,
                    "End" => applied().max,
                    _ => return,
                };
                event.prevent_default();
                widths.set(pane, width);
                widths.save(pane);
            }></div>
    })
}
