use leptos::prelude::*;
use serde::{Deserialize, Serialize};
use web_sys::Storage;

const KEY: &str = "minarbot-presentation-v1";
const READ_ERROR: &str =
    "Cannot read saved settings. Using the default view; choose a view to try saving again.";
const SAVE_ERROR: &str = "Appearance has not been saved. Allow browser storage and try again.";

const MODES: [(&str, &str); 3] = [("light", "Light"), ("dark", "Dark"), ("system", "System")];
const SIZES: [(&str, &str); 4] = [
    ("compact", "Compact"),
    ("standard", "Default"),
    ("large", "Large"),
    ("larger", "Larger"),
];

#[derive(Clone, Deserialize)]
pub struct ColorTheme {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Presentation {
    version: u8,
    #[serde(default = "default_color")]
    color_theme: String,
    theme: String,
    text_size: String,
}

fn default_color() -> String {
    "minarbot".into()
}

impl Default for Presentation {
    fn default() -> Self {
        Self {
            version: 1,
            color_theme: default_color(),
            theme: "dark".into(),
            text_size: "standard".into(),
        }
    }
}

fn storage() -> Option<Storage> {
    window().local_storage().ok().flatten()
}

fn read(themes: &[ColorTheme]) -> Option<Presentation> {
    let Some(raw) = storage()?.get_item(KEY).ok()? else {
        return Some(Presentation::default());
    };
    let value: Presentation = serde_json::from_str(&raw).ok()?;
    (value.version == 1
        && themes.iter().any(|theme| theme.id == value.color_theme)
        && MODES.iter().any(|(id, _)| *id == value.theme)
        && SIZES.iter().any(|(id, _)| *id == value.text_size))
    .then_some(value)
}

fn apply(value: &Presentation) {
    if let Some(root) = document().document_element() {
        let _ = root.set_attribute("data-theme", &value.theme);
        let _ = root.set_attribute("data-text-size", &value.text_size);
        let _ = root.set_attribute("data-color-theme", &value.color_theme);
    }
}

pub fn restore(themes: &[ColorTheme]) {
    apply(&read(themes).unwrap_or_default());
}

#[derive(Clone, Copy)]
struct Controls {
    value: RwSignal<Presentation>,
    error: RwSignal<&'static str>,
    notice: RwSignal<&'static str>,
}

impl Controls {
    fn apply_saved(self, saved: Option<Presentation>) {
        self.error
            .set(if saved.is_some() { "" } else { READ_ERROR });
        let value = saved.unwrap_or_default();
        apply(&value);
        self.value.set(value);
    }

    fn change(self, edit: impl FnOnce(&mut Presentation)) {
        let mut next = self.value.get_untracked();
        edit(&mut next);
        let saved = serde_json::to_string(&next)
            .ok()
            .and_then(|raw| storage()?.set_item(KEY, &raw).ok());
        if saved.is_some() {
            self.apply_saved(Some(next));
            self.notice.set("Appearance saved in this browser.");
        } else {
            self.error.set(SAVE_ERROR);
            self.notice.set("");
        }
    }
}

fn choice_row(
    title: &'static str,
    options: &'static [(&'static str, &'static str)],
    controls: Controls,
    field: fn(&mut Presentation) -> &mut String,
) -> impl IntoView {
    view! {
        <div class="k-row">
            <div class="k-row__main"><span class="k-row__text"><span class="k-row__title">{title}</span></span></div>
            <span class="k-row__trailing">
                <div class="k-choice" role="radiogroup" aria-label=title
                    on:keydown=move |event| super::settings::move_focus(
                        &event, "div[role=radiogroup]", true,
                    )>
                    {options.iter().map(|&(id, label)| {
                        let active = move || field(&mut controls.value.get()) == id;
                        view! {
                            <button type="button" class="k-choice__opt" class:is-active=active
                                role="radio" aria-checked=move || if active() { "true" } else { "false" } tabindex=move || if active() { 0 } else { -1 }
                                on:click=move |_| controls.change(|value| *field(value) = id.into())>
                                <span class="k-choice__title">{label}</span>
                            </button>
                        }
                    }).collect_view()}
                </div>
            </span>
        </div>
    }
}

pub fn panel(themes: Vec<ColorTheme>, error_icon: String) -> impl IntoView {
    let controls = Controls {
        value: RwSignal::new(Presentation::default()),
        error: RwSignal::new(""),
        notice: RwSignal::new(""),
    };
    controls.apply_saved(read(&themes));
    let cards = themes.iter().map(|theme| {
        let chosen = theme.id.clone();
        let id = theme.id.clone();
        view! {
            <button type="button" class="color-theme-card"
                aria-pressed=move || controls.value.with(|value| if value.color_theme == id { "true" } else { "false" })
                on:click=move |_| controls.change(|value| value.color_theme.clone_from(&chosen))>
                <span class="theme-preview" data-color-theme=theme.id.clone()
                    data-theme=move || controls.value.with(|value| value.theme.clone()) aria-hidden="true">
                    <span class="theme-preview__sidebar"><i></i><i></i><i></i></span>
                    <span class="theme-preview__chat"><i></i><b></b><i></i></span>
                </span>
                <span class="color-theme-card__name">{theme.label.clone()}</span>
            </button>
        }
    }).collect_view();
    let listener = window_event_listener(leptos::ev::storage, move |event| {
        if event.key().is_none_or(|key| key == KEY) && event.storage_area() == storage() {
            controls.apply_saved(read(&themes));
            controls.notice.set("Appearance updated from another tab.");
        }
    });
    on_cleanup(move || listener.remove());

    view! {
        <div class="k-page bs-root">
            <header class="k-page-head">
                <div class="k-page-head__text"><h1 id="appearance-title" tabindex="-1">"Appearance"</h1></div>
            </header>
            <section class="k-section">
                <div class="k-section__head"><h2 class="k-section__title">"Theme"</h2></div>
                <div class="color-theme-picker" role="group" aria-label="Color theme">{cards}</div>
            </section>
            <div class="k-card">
                {choice_row("Mode", &MODES, controls, |value| &mut value.theme)}
                {choice_row("Text size", &SIZES, controls, |value| &mut value.text_size)}
            </div>
            <div class="k-alert is-error" role="alert" hidden=move || controls.error.get().is_empty()>
                <span class="appearance-icon" aria-hidden="true" inner_html=error_icon></span>
                <div class="k-alert__body"><div class="k-alert__text">{controls.error}</div></div>
            </div>
            <span class="sr-only" role="status">{controls.notice}</span>
        </div>
    }
}
