use super::{actions, error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Deserialize, Serialize)]
#[serde(tag = "name", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Action {
    Snapshot,
    Click {
        r#ref: String,
    },
    Type {
        r#ref: String,
        text: String,
        #[serde(default)]
        submit: bool,
    },
    Select {
        r#ref: String,
        value: String,
    },
    Hover {
        r#ref: String,
    },
    Press {
        key: String,
    },
    Navigate {
        url: String,
    },
    Back,
    Tabs {
        action: TabAction,
        tab: Option<String>,
        url: Option<String>,
        profile: Option<String>,
    },
    Wait {
        text: Option<String>,
        r#ref: Option<String>,
        network_idle: Option<bool>,
        timeout_ms: Option<u64>,
    },
    Extract {
        r#ref: Option<String>,
    },
    Screenshot {
        r#ref: Option<String>,
    },
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum TabAction {
    List,
    Open,
    Switch,
    Close,
}

// Separate from public arguments so callers cannot request internal secret handling.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub action: Action,
    pub redacted: bool,
    pub epoch: u64,
}

#[derive(Deserialize, Serialize)]
pub(super) struct Reply {
    pub result: Value,
    pub image: Option<String>,
}

impl Action {
    pub fn read_only(&self) -> bool {
        matches!(
            self,
            Self::Snapshot
                | Self::Wait { .. }
                | Self::Extract { .. }
                | Self::Screenshot { .. }
                | Self::Tabs {
                    action: TabAction::List,
                    ..
                }
        )
    }

    pub fn parse(name: &str, args: &Value) -> Result<Self> {
        let mut object = args.as_object().cloned().ok_or_else(invalid)?;
        let name = name.strip_prefix("browser_").ok_or_else(invalid)?;
        if object.insert("name".into(), json!(name)).is_some()
            || object.values().any(Value::is_null)
        {
            return Err(invalid());
        }
        let mut action: Self =
            serde_json::from_value(Value::Object(object)).map_err(|_| invalid())?;
        action.validate()?;
        Ok(action)
    }

    pub fn validate(&mut self) -> Result<()> {
        let reference = |value: &str| {
            bounded(value, 48)
                && value
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        };
        let valid = match self {
            Self::Click { r#ref } | Self::Hover { r#ref } => reference(r#ref),
            Self::Type { r#ref, text, .. } => reference(r#ref) && bounded(text, 4000),
            Self::Select { r#ref, value } => {
                reference(r#ref) && (value.is_empty() || bounded(value, 2000))
            }
            Self::Press { key } => key_code(key).is_some(),
            Self::Navigate { url } => {
                *url = actions::http_url(url)?;
                true
            }
            Self::Tabs {
                action,
                tab,
                url,
                profile,
            } => {
                if profile.as_deref().is_some_and(|name| name != "Default") {
                    return Err(error(
                        400,
                        "Unknown browser profile. This Computer uses its own Default profile.",
                    ));
                }
                if let Some(url) = url {
                    *url = actions::http_url(url)?;
                }
                match action {
                    TabAction::List => tab.is_none() && url.is_none(),
                    TabAction::Open => tab.is_none(),
                    TabAction::Switch | TabAction::Close => {
                        url.is_none() && tab.as_deref().is_some_and(reference)
                    }
                }
            }
            Self::Wait {
                text,
                r#ref,
                network_idle,
                timeout_ms,
            } => {
                usize::from(text.is_some())
                    + usize::from(r#ref.is_some())
                    + usize::from(*network_idle == Some(true))
                    == 1
                    && *network_idle != Some(false)
                    && text.as_deref().is_none_or(|text| bounded(text, 500))
                    && r#ref.as_deref().is_none_or(reference)
                    && timeout_ms.is_none_or(|ms| (1..=8000).contains(&ms))
            }
            Self::Extract { r#ref } | Self::Screenshot { r#ref } => {
                r#ref.as_deref().is_none_or(reference)
            }
            Self::Snapshot | Self::Back => true,
        };
        if valid {
            Ok(())
        } else {
            Err(invalid())
        }
    }
}

fn invalid() -> crate::web::Failure {
    error(400, "Invalid browser arguments. Check the tool parameters.")
}

fn bounded(value: &str, length: usize) -> bool {
    !value.is_empty() && value.encode_utf16().count() <= length && !value.contains('\0')
}

pub(super) fn key_code(key: &str) -> Option<(u8, String, String, u8)> {
    if key.len() > 32 {
        return None;
    }
    let mut modifiers = 0;
    let mut parts = key.split('+').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_some() {
            let flag = match part {
                "Alt" => 1,
                "Ctrl" => 2,
                "Meta" => 4,
                "Shift" => 8,
                _ => return None,
            };
            if modifiers & flag != 0 {
                return None;
            }
            modifiers |= flag;
            continue;
        }
        let (code, keyboard) = match part {
            "Backspace" => (8, part),
            "Tab" => (9, part),
            "Enter" => (13, part),
            "Escape" => (27, part),
            "Space" => (32, " "),
            "PageUp" => (33, part),
            "PageDown" => (34, part),
            "End" => (35, part),
            "Home" => (36, part),
            "ArrowLeft" => (37, part),
            "ArrowUp" => (38, part),
            "ArrowRight" => (39, part),
            "ArrowDown" => (40, part),
            "Delete" => (46, part),
            _ if part.len() == 1 && part.as_bytes()[0].is_ascii_alphanumeric() => {
                let byte = part.as_bytes()[0].to_ascii_uppercase();
                let character = if modifiers & 8 == 0 {
                    byte.to_ascii_lowercase()
                } else {
                    byte
                };
                let prefix = if byte.is_ascii_digit() {
                    "Digit"
                } else {
                    "Key"
                };
                return Some((
                    modifiers,
                    String::from(char::from(character)),
                    format!("{prefix}{}", char::from(byte)),
                    byte,
                ));
            }
            _ => return None,
        };
        let physical = if code == 32 { "Space" } else { part };
        return Some((modifiers, keyboard.into(), physical.into(), code));
    }
    None
}

pub(super) fn tools() -> Vec<Value> {
    let reference = json!({"type":"string","maxLength":48});
    let url = json!({"type":"string","maxLength":2000});
    let reference_args = json!({"ref":reference});
    let tool = |name, description, properties, required| json!({"type":"function","function":{"name":name,"description":description,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}}});
    vec![
        tool("browser_snapshot", "Read a bounded accessibility tree of the visible Chrome tab. Use element refs from this snapshot.", json!({}), json!([])),
        tool("browser_click", "Click an element from the current snapshot.", reference_args.clone(), json!(["ref"])),
        tool("browser_type", "Type literal text into an editable element. Set submit to submit its form or press Enter.", json!({"ref":reference,"text":{"type":"string","minLength":1,"maxLength":4000},"submit":{"type":"boolean"}}), json!(["ref","text"])),
        tool("browser_select", "Select an option by value in a native select element.", json!({"ref":reference,"value":{"type":"string","maxLength":2000}}), json!(["ref","value"])),
        tool("browser_hover", "Hover an element from the current snapshot.", reference_args.clone(), json!(["ref"])),
        tool("browser_press", "Press a key in the active page. Supports letters, digits, Enter, Tab, Escape, Space, Backspace, Delete, arrows, Home, End, PageUp and PageDown, with optional Ctrl+, Alt+, Shift+ or Meta+.", json!({"key":{"type":"string","maxLength":32}}), json!(["key"])),
        tool("browser_navigate", "Navigate the active Chrome tab to an HTTP or HTTPS URL.", json!({"url":url}), json!(["url"])),
        tool("browser_back", "Go back one entry in the active tab's history.", json!({}), json!([])),
        tool("browser_tabs", "List, open, switch or close Chrome tabs. Switch and close take an ID from list. Open optionally takes an HTTP or HTTPS URL.", json!({"action":{"type":"string","enum":["list","open","switch","close"]},"tab":{"type":"string","maxLength":48},"url":url,"profile":{"type":"string","enum":["Default"]}}), json!(["action"])),
        tool("browser_wait", "Wait for one condition in the active tab: visible text, a visible element ref, or 500 ms of network idle. Timeout defaults to 5 seconds.", json!({"text":{"type":"string","minLength":1,"maxLength":500},"ref":reference,"network_idle":{"type":"boolean"},"timeout_ms":{"type":"integer","minimum":1,"maximum":8000}}), json!([])),
        tool("browser_extract", "Read bounded visible text from the current page or an element ref.", reference_args.clone(), json!([])),
        tool("browser_screenshot", "Capture the active Chrome viewport or the visible part of an element by ref, as a PNG image.", reference_args, json!([])),
    ]
}
