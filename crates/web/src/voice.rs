use crate::general::{Row, Section, Switch};
use leptos::prelude::*;
use wasm_bindgen::{closure::Closure, JsCast};
use web_sys::{Event, SpeechSynthesis, SpeechSynthesisUtterance, SpeechSynthesisVoice};

const ENABLED: &str = "minarbot.speech.enabled.v1";
const VOICE: &str = "minarbot.speech.voice.v1";

fn key(voice: &SpeechSynthesisVoice) -> String {
    serde_json::json!([voice.voice_uri(), voice.name(), voice.lang()]).to_string()
}

fn voices(engine: &SpeechSynthesis) -> impl Iterator<Item = SpeechSynthesisVoice> {
    engine
        .get_voices()
        .to_vec()
        .into_iter()
        .filter_map(|value| value.dyn_into::<SpeechSynthesisVoice>().ok())
        .filter(SpeechSynthesisVoice::local_service)
}

fn preference() -> Option<(bool, String)> {
    let storage = window().local_storage().ok().flatten()?;
    let enabled = storage
        .get_item(ENABLED)
        .ok()?
        .map_or(Some(false), |raw| serde_json::from_str(&raw).ok())?;
    let voice = storage
        .get_item(VOICE)
        .ok()?
        .filter(|voice| voice.encode_utf16().count() <= 2000)
        .unwrap_or_default();
    Some((enabled, voice))
}

pub fn settings() -> impl IntoView {
    let info_icon = use_context::<crate::shell::Icons>()
        .map(|icons| icons.info)
        .unwrap_or_default();
    let voice_icon = use_context::<crate::shell::Icons>()
        .map(|icons| icons.voice)
        .unwrap_or_default();
    let Some(engine) = window()
        .speech_synthesis()
        .ok()
        .filter(|engine| !engine.is_undefined())
    else {
        return ().into_any();
    };
    let engine = StoredValue::new_local(engine);
    let options = RwSignal::new(Vec::<(String, String)>::new());
    let current = RwSignal::new((false, String::new()));
    let error = RwSignal::new("");
    let playing = RwSignal::new(false);
    let utterance = StoredValue::new_local(None::<SpeechSynthesisUtterance>);
    let refresh = move || {
        engine.with_value(|engine| {
            options.set(
                voices(engine)
                    .map(|voice| (key(&voice), format!("{} ({})", voice.name(), voice.lang())))
                    .collect(),
            );
        })
    };
    refresh();
    let changed = Closure::<dyn FnMut(Event)>::new(move |_| refresh());
    engine.with_value(|engine| engine.set_onvoiceschanged(Some(changed.as_ref().unchecked_ref())));
    let changed = StoredValue::new_local(changed);

    let stop = move || {
        if let Some(speech) = utterance.try_update_value(Option::take).flatten() {
            speech.set_onend(None);
            speech.set_onerror(None);
            engine.with_value(SpeechSynthesis::cancel);
        }
        // Cleanup calls this too: a write there would rerun disposed views.
        if playing.try_get_untracked() == Some(true) {
            playing.try_set(false);
        }
    };
    let sync = move || {
        stop();
        let value = preference();
        error.set(if value.is_some() {
            ""
        } else {
            "Voice settings could not be read. Allow browser storage and try again."
        });
        current.set(value.unwrap_or_default());
    };
    sync();
    let storage = window_event_listener(leptos::ev::storage, move |event| {
        if event.key().is_none_or(|key| key == ENABLED || key == VOICE)
            && event.storage_area() == window().local_storage().ok().flatten()
        {
            sync();
        }
    });
    let pagehide = window_event_listener(leptos::ev::pagehide, move |_| stop());
    let finish = StoredValue::new_local(Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        if event.type_() == "error" {
            error.try_set("Voice unavailable. Pick a voice in Settings > General.");
        }
        playing.try_set(false);
    }));
    on_cleanup(move || {
        storage.remove();
        pagehide.remove();
        stop();
        engine.with_value(|engine| engine.set_onvoiceschanged(None));
        changed.dispose();
    });

    let save = move |key, value: &str| {
        let saved = window()
            .local_storage()
            .ok()
            .flatten()
            .and_then(|storage| storage.set_item(key, value).ok())
            .is_some();
        sync();
        if !saved {
            current.set((false, String::new()));
            error.set("Voice settings could not be saved. Allow browser storage and try again.");
        }
    };
    let test = move |_| {
        let was_playing = playing.get_untracked();
        stop();
        if was_playing {
            return;
        }
        let selected = current.with_untracked(|value| value.1.clone());
        engine.with_value(|engine| {
            let voice = voices(engine).find(|voice| key(voice) == selected);
            if let (Some(voice), Ok(speech)) = (
                voice,
                SpeechSynthesisUtterance::new_with_text("This is your selected voice in Minarbot."),
            ) {
                error.set("");
                speech.set_voice(Some(&voice));
                speech.set_lang(&voice.lang());
                finish.with_value(|handler| {
                    speech.set_onend(Some(handler.as_ref().unchecked_ref()));
                    speech.set_onerror(Some(handler.as_ref().unchecked_ref()));
                });
                playing.set(true);
                utterance.set_value(Some(speech.clone()));
                engine.speak(&speech);
            } else {
                error.set("Voice unavailable. Pick a voice in Settings > General.");
            }
        });
    };
    let selected = Memo::new(move |_| {
        current.with(|value| {
            options.with(|options| {
                options
                    .iter()
                    .any(|(key, _)| key == &value.1)
                    .then(|| value.1.clone())
            })
        })
    });
    view! {
        <div hidden=move || options.with(Vec::is_empty)>
            <Section title="Voice">
                <div class="k-card">
                    <Row title="Read aloud">
                        <button type="button" class="k-icon-btn" aria-label="About read aloud" title="Adds Listen to replies. Local voices only.">
                            <span class="appearance-icon" inner_html=info_icon.clone()></span>
                        </button>
                        <Switch label="Read aloud" checked=Signal::derive(move || current.with(|value| value.0))
                            disabled=Signal::derive(|| false) on_change=move |enabled| save(ENABLED, if enabled { "true" } else { "false" }) />
                    </Row>
                    <div class="k-row" hidden=move || !current.with(|value| value.0)>
                        <div class="k-row__main"><span class="k-row__text">
                            <span class="k-row__title"><label for="local-voice">"Voice"</label></span>
                            <span class="k-row__meta" hidden=move || current.with(|value| value.1.is_empty()) || selected.with(Option::is_some)>"Saved voice unavailable."</span>
                        </span></div>
                        <span class="k-row__trailing"><span class="k-row-actions">
                            <select id="local-voice" class="k-input k-btn--sm" prop:value=move || selected.get().unwrap_or_default()
                                on:change=move |event| save(VOICE, &event_target_value(&event))>
                                <option value="" disabled selected=move || selected.with(Option::is_none)>"Choose voice"</option>
                                {move || options.get().into_iter().map(|(key, label)| {
                                    let option = key.clone();
                                    view! { <option value=key selected=move || selected.with(|value| value.as_ref() == Some(&option))>{label}</option> }
                                }).collect_view()}
                            </select>
                            <button type="button" class="k-btn k-btn--sm k-btn--ghost"
                                aria-label=move || if playing.get() { "Stop voice test" } else { "Test voice" }
                                disabled=move || selected.with(Option::is_none) on:click=test>
                                <span class="appearance-icon" inner_html=voice_icon.clone()></span>
                                {move || if playing.get() { "Stop" } else { "Test" }}
                            </button>
                        </span></span>
                    </div>
                </div>
                <p class="k-field__error" role="status" hidden=move || error.get().is_empty()>{error}</p>
            </Section>
        </div>
    }.into_any()
}
