use super::{error, Result};
use serde_json::{json, Value};

pub fn validate(name: &str, value: &Value, width: u32, height: u32) -> Result<Vec<String>> {
    let (allowed, required): (&[&str], &[&str]) = match name {
        "computer_click" => (&["x", "y", "button"], &["x", "y"]),
        "computer_type" => (&["text"], &["text"]),
        "computer_key" => (&["keys"], &["keys"]),
        "computer_scroll" => (&["dx", "dy"], &["dx", "dy"]),
        "computer_open_url" => (&["url"], &["url"]),
        "computer_request_user" => (&["reason"], &["reason"]),
        "computer_list_vault" | "computer_screenshot" => (&[], &[]),
        "computer_fill_secret" => (&["entry", "which", "ref"], &["entry", "which"]),
        "computer_request_secret" => (&["entry", "origin"], &["entry", "origin"]),
        _ => return Err(error(400, "Invalid computer action.")),
    };
    let object = value
        .as_object()
        .ok_or_else(|| error(400, "Invalid computer action."))?;
    if object.keys().any(|k| !allowed.contains(&k.as_str()))
        || required.iter().any(|k| !object.contains_key(*k))
    {
        return Err(error(400, "Invalid computer arguments."));
    }
    let args = match name {
        "computer_click" => {
            let x = value["x"]
                .as_i64()
                .filter(|v| (0..width as i64).contains(v));
            let y = value["y"]
                .as_i64()
                .filter(|v| (0..height as i64).contains(v));
            let button = value.get("button").unwrap_or(&Value::Null).as_i64();
            if x.is_none()
                || y.is_none()
                || object.contains_key("button") && !button.is_some_and(|v| (1..=3).contains(&v))
            {
                return Err(error(400, "Invalid click coordinates or button."));
            }
            vec![
                "mousemove".into(),
                value["x"].to_string(),
                value["y"].to_string(),
                "click".into(),
                button.unwrap_or(1).to_string(),
            ]
        }
        "computer_type" => {
            let text = value["text"]
                .as_str()
                .filter(|s| !s.is_empty() && s.encode_utf16().count() <= 4000 && !s.contains('\0'))
                .ok_or_else(|| error(400, "Type 1-4000 characters without null bytes."))?;
            vec![
                "type".into(),
                "--clearmodifiers".into(),
                "--delay".into(),
                "0".into(),
                "--".into(),
                text.into(),
            ]
        }
        "computer_key" => {
            let modifiers = ["Control_L", "Alt_L", "Shift_L", "Super_L"];
            let keys = value["keys"]
                .as_array()
                .filter(|v| (1..=5).contains(&v.len()))
                .ok_or_else(|| error(400, "Unsupported key."))?;
            let mut names = Vec::new();
            for v in keys {
                let key = v.as_str().ok_or_else(|| error(400, "Unsupported key."))?;
                if !(key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric()
                    || modifiers.contains(&key)
                    || [
                        "Return",
                        "Escape",
                        "Tab",
                        "space",
                        "BackSpace",
                        "Delete",
                        "Up",
                        "Down",
                        "Left",
                        "Right",
                        "Home",
                        "End",
                        "Page_Up",
                        "Page_Down",
                    ]
                    .contains(&key)
                    || key
                        .strip_prefix('F')
                        .and_then(|n| n.parse::<u8>().ok())
                        .is_some_and(|n| (1..=12).contains(&n) && key == format!("F{n}")))
                {
                    return Err(error(400, "Unsupported key."));
                }
                names.push(key);
            }
            if names[..names.len() - 1]
                .iter()
                .any(|k| !modifiers.contains(k))
                || names.last().is_some_and(|k| modifiers.contains(k))
            {
                return Err(error(400, "Use modifiers followed by one key."));
            }
            vec!["key".into(), "--clearmodifiers".into(), names.join("+")]
        }
        "computer_scroll" => {
            let mut result = Vec::new();
            for (axis, positive, negative) in [("dx", 7, 6), ("dy", 5, 4)] {
                let n = value[axis]
                    .as_i64()
                    .filter(|n| (-20..=20).contains(n))
                    .ok_or_else(|| error(400, "Scroll ticks must be integers from -20 to 20."))?;
                if n != 0 {
                    result.extend([
                        "click".into(),
                        "--repeat".into(),
                        n.abs().to_string(),
                        if n > 0 { positive } else { negative }.to_string(),
                    ]);
                }
            }
            result
        }
        "computer_open_url" => {
            vec![http_url(
                value["url"]
                    .as_str()
                    .ok_or_else(|| error(400, "Invalid URL."))?,
            )?]
        }
        "computer_fill_secret" | "computer_request_secret" => {
            let entry = value["entry"]
                .as_str()
                .filter(|s| super::vault::valid_name(s))
                .ok_or_else(|| error(400, "Invalid Vault entry name."))?;
            let target = if name == "computer_fill_secret" {
                value["which"]
                    .as_str()
                    .filter(|s| ["username", "password", "totp", "value"].contains(s))
                    .ok_or_else(|| error(400, "Invalid Vault field."))?
                    .to_owned()
            } else {
                super::vault::origin(value["origin"].as_str().unwrap_or_default())?
            };
            let mut args = vec![entry.into(), target];
            if let Some(reference) = value.get("ref") {
                args.push(
                    reference
                        .as_str()
                        .filter(|r| super::browser_actions::valid_ref(r))
                        .ok_or_else(|| error(400, "Invalid browser element ref."))?
                        .to_owned(),
                );
            }
            args
        }
        "computer_request_user" => {
            value["reason"]
                .as_str()
                .filter(|s| {
                    !s.is_empty()
                        && s.encode_utf16().count() <= 500
                        && !s.chars().any(|c| c <= '\u{1f}' || c == '\u{7f}')
                })
                .ok_or_else(|| error(400, "Reason must be 1-500 printable characters."))?;
            Vec::new()
        }
        _ => Vec::new(),
    };
    Ok(args)
}
pub(super) fn http_url(s: &str) -> Result<String> {
    if s.encode_utf16().count() > 2000 || s.chars().any(|c| c <= ' ' || c == '\u{7f}') {
        return Err(error(400, "Invalid URL."));
    }
    let url = url::Url::parse(s).map_err(|_| error(400, "Invalid URL."))?;
    if !["http", "https"].contains(&url.scheme())
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(error(400, "Use an http/https URL without credentials."));
    }
    Ok(url.to_string())
}
pub(crate) fn is_computer_tool(name: &str) -> bool {
    name.starts_with("computer_") || name.starts_with("browser_")
}
pub(crate) fn is_read_tool(name: &str) -> bool {
    matches!(
        name,
        "computer_screenshot"
            | "browser_snapshot"
            | "browser_wait"
            | "browser_extract"
            | "browser_screenshot"
    )
}
pub fn tools(width: u32, height: u32) -> Value {
    let tool = |name: &str, description: String, properties: Value, required: Value| json!({"type":"function","function":{"name":name,"description":description,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}}});
    let entry = json!({"type":"string","pattern":"^[a-z0-9-]{1,64}$"});
    let mut tools = vec![
        tool("computer_screenshot",format!("See your own {width}x{height} desktop, returned as a PNG image scaled to 960x600. Input coordinates use original {width}x{height} pixels. Starts your desktop on demand."),json!({}),json!([])),
        tool("computer_click","Click your desktop at original screen coordinates.".into(),json!({"x":{"type":"integer","minimum":0,"maximum":width-1},"y":{"type":"integer","minimum":0,"maximum":height-1},"button":{"type":"integer","enum":[1,2,3],"description":"1 left, 2 middle, 3 right."}}),json!(["x","y"])),
        tool("computer_type","Type literal text into the focused field on your desktop.".into(),json!({"text":{"type":"string","minLength":1,"maxLength":4000}}),json!(["text"])),
        tool("computer_key","Press a key or chord on your desktop. Keys: single ASCII letters/digits, Return, Escape, Tab, space, BackSpace, Delete, arrows, Home, End, Page_Up, Page_Down, F1-F12; modifiers Control_L, Alt_L, Shift_L, Super_L.".into(),json!({"keys":{"type":"array","minItems":1,"maxItems":5,"items":{"type":"string"}}}),json!(["keys"])),
        tool("computer_scroll","Scroll your desktop in wheel ticks: positive dx right, positive dy down. Each axis -20..20.".into(),json!({"dx":{"type":"integer","minimum":-20,"maximum":20},"dy":{"type":"integer","minimum":-20,"maximum":20}}),json!(["dx","dy"])),
        tool("computer_open_url","Navigate Chrome on your desktop to an http/https URL.".into(),json!({"url":{"type":"string","maxLength":2000}}),json!(["url"])),
        tool("computer_request_user","Ask the user to take control for a login, CAPTCHA, payment, or other human-only step.".into(),json!({"reason":{"type":"string","minLength":1,"maxLength":500}}),json!(["reason"])),
        tool("computer_list_vault","List the Vault entry names, site origins and fields granted to you.".into(),json!({}),json!([])),
        tool("computer_fill_secret","Fill a granted Vault field on its saved origin. Pass a browser snapshot ref or focus the intended Chrome input. Browser reads and screenshots are hidden after a fill. Refer to the placeholder and keep values out of chat.".into(),json!({"entry":entry,"which":{"type":"string","enum":["username","password","totp","value"]},"ref":{"type":"string","maxLength":48}}),json!(["entry","which"])),
        tool("computer_request_secret","Ask the owner for a missing value in a secure Minarbot form. First focus the intended Chrome input. If approved, save a new Vault entry and fill the value. Use an unused entry name and the site's HTTPS origin.".into(),json!({"entry":entry,"origin":{"type":"string","maxLength":2000}}),json!(["entry","origin"]))
    ];
    tools.extend(super::browser_actions::tools());
    json!(tools)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn input_contract() -> Result<()> {
        assert_eq!(
            validate("computer_click", &json!({"x":1279,"y":799}), 1280, 800)?,
            ["mousemove", "1279", "799", "click", "1"]
        );
        for (action, args) in [
            ("computer_click", json!({"x":1280,"y":0})),
            ("computer_click", json!({"x":0,"y":-1})),
            ("computer_click", json!({"x":0.5,"y":2})),
            ("computer_click", json!({"x":0,"y":0,"button":"1"})),
            ("computer_click", json!({"x":0,"y":0,"display":":0"})),
            ("computer_key", json!({"keys":["--window"]})),
            ("computer_key", json!({"keys":["exec"]})),
            ("computer_key", json!({"keys":["Control_L"]})),
            ("computer_key", json!({"keys":["a","b"]})),
            ("computer_type", json!({"text":"x\0"})),
            ("computer_type", json!({"text":"x".repeat(4001)})),
            ("computer_scroll", json!({"dx":21,"dy":0})),
            ("computer_scroll", json!({"dx":0,"dy":null})),
            ("computer_open_url", json!({"url":"file:///etc/passwd"})),
            ("computer_open_url", json!({"url":"javascript:alert(1)"})),
            (
                "computer_open_url",
                json!({"url":"https://user:secret@example.com"}),
            ),
            ("computer_open_url", json!({"url":"http://user@localhost"})),
            ("computer_screenshot", json!({"agent":"foreign"})),
            ("unknown", json!({})),
        ] {
            assert!(
                validate(action, &args, 1280, 800).is_err(),
                "{action} {args}"
            );
        }
        assert_eq!(
            validate("computer_click", &json!({"x":1919,"y":1199}), 1920, 1200)?,
            ["mousemove", "1919", "1199", "click", "1"]
        );
        assert!(validate("computer_click", &json!({"x":1024,"y":0}), 1024, 640).is_err());
        assert_eq!(
            validate(
                "computer_key",
                &json!({"keys":["Control_L","a"]}),
                1280,
                800
            )?,
            ["key", "--clearmodifiers", "Control_L+a"]
        );
        assert!(validate("computer_key", &json!({"keys":["a","Return"]}), 1280, 800).is_err());
        let text = "--window 0; $(anything)";
        let args = validate("computer_type", &json!({"text":text}), 1280, 800)?;
        assert_eq!(args[args.len() - 2..], ["--", text]);
        assert_eq!(
            validate("computer_scroll", &json!({"dx":-2,"dy":3}), 1280, 800)?,
            ["click", "--repeat", "2", "6", "click", "--repeat", "3", "5"]
        );
        Ok(())
    }
}
