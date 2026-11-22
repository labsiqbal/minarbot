use leptos::prelude::*;

pub fn page(error: String) -> impl IntoView {
    view! {
        <main>
            <h1>"Pair this device"</h1>
            <p>"Get a code from Settings > Devices on a browser that can open Minarbot."</p>
            {(!error.is_empty()).then(|| view! { <p role="alert">{error}</p> })}
            <form method="post" action="/pair">
                <label for="pair-code">"Pairing code"</label>
                <input id="pair-code" name="code" inputmode="numeric" autocomplete="one-time-code"
                    pattern="[0-9]{6}" maxlength="6" required />
                <label for="pair-name">"Device name"</label>
                <input id="pair-name" name="name" maxlength="64" required />
                <button type="submit">"Pair device"</button>
            </form>
        </main>
    }
}
