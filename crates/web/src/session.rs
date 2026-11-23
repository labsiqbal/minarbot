use leptos::prelude::*;

pub(crate) fn reload() {
    // Hide the old view immediately, including while a new document is loading.
    if let Some(root) = document().get_element_by_id("leptos-root") {
        let _ = root.set_attribute("hidden", "");
    }
    let _ = window().location().reload();
}

#[cfg(not(feature = "hub"))]
pub(crate) fn watch_device() {
    #[derive(serde::Deserialize)]
    struct Device {
        current: Option<String>,
    }

    let previous = StoredValue::new(None::<Option<String>>);
    let busy = RwSignal::new(false);
    let failed = RwSignal::new(false);
    let refresh = move || {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        leptos::task::spawn_local(async move {
            let result = crate::api::read_status::<Device>("/api/device-pairing").await;
            if busy.try_set(false).is_some() {
                return;
            }
            failed.set(result.is_err());
            match result {
                Ok(device) => {
                    if previous.with_value(|id| id.as_ref().is_some_and(|id| *id != device.current)) {
                        reload();
                    } else {
                        previous.set_value(Some(device.current));
                    }
                }
                Err((401, _)) => reload(),
                Err(_) => {}
            }
        });
    };
    refresh();
    crate::connection_ui::poll_with_backoff(5000, move || (!busy.get_untracked()).then(|| failed.get_untracked()), refresh);
}
