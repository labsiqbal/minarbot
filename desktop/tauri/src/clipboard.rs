use crate::connection::{same_origin, Result};
use gtk::{
    gdk::{Atom, ModifierType},
    glib::{base64_encode, Propagation},
    prelude::WidgetExt,
    Clipboard,
};
use tauri::{Url, WebviewWindow};
use webkit2gtk::WebViewExt;

pub fn install(window: &WebviewWindow, configured: &Url) -> Result<()> {
    let view = window.clone();
    let origin = serde_json::to_string(&configured.origin().ascii_serialization())?;
    let configured = configured.clone();
    window.with_webview(move |native| {
        native.inner().connect_key_press_event(move |widget, event| {
            // WebKitGTK skips image-only clipboard contents in a plain textarea.
            if !event.state().contains(ModifierType::CONTROL_MASK)
                || event.state().intersects(ModifierType::SHIFT_MASK | ModifierType::MOD1_MASK)
                || event.keyval().to_unicode().is_none_or(|key| !key.eq_ignore_ascii_case(&'v'))
                || widget.uri().and_then(|value| Url::parse(&value).ok())
                    .is_none_or(|current| !same_origin(&current, &configured))
            {
                return Propagation::Proceed;
            }
            let Some(clipboard) = Clipboard::default(&widget.display()) else {
                return Propagation::Proceed;
            };
            let png = Atom::intern("image/png");
            if !clipboard.wait_is_target_available(&png) || clipboard.wait_is_text_available() {
                return Propagation::Proceed;
            }
            let view = view.clone();
            let origin = origin.clone();
            clipboard.request_contents(&png, move |_, image| {
                if !view.is_focused().unwrap_or(false)
                    || !(1..=16 * 1024 * 1024).contains(&image.length())
                {
                    return;
                }
                let bytes = image.data();
                if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                    return;
                }
                let encoded = base64_encode(&bytes);
                let _ = view.eval(format!(r#"
                    (() => {{
                        if (window !== top || location.origin !== {origin} || !document.hasFocus()) return;
                        const field = document.activeElement;
                        if (!(field instanceof HTMLTextAreaElement) || field.readOnly || field.disabled) return;
                        const data = new DataTransfer();
                        const bytes = Uint8Array.from(atob("{encoded}"), (byte) => byte.charCodeAt(0));
                        data.items.add(new File([bytes], "clipboard.png", {{ type: "image/png" }}));
                        field.dispatchEvent(new ClipboardEvent("paste", {{ clipboardData: data, bubbles: true, cancelable: true }}));
                    }})();
                "#));
            });
            Propagation::Stop
        });
    })?;
    Ok(())
}
