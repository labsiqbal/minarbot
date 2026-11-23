use crate::api::command_id;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_MESSAGE: usize = 16_000;
pub const MAX_FILES: usize = 4;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

pub fn id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Partial {
    pub seq: u64,
    pub text: String,
    model: String,
    pub status: String,
    pub segment: Option<String>,
    revision: Option<u64>,
    #[serde(rename = "final")]
    final_frame: Option<bool>,
}

#[derive(Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub partial: Partial,
}

pub fn parse_snapshot(raw: &str) -> Option<Snapshot> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let partial = value.get("partial")?.as_object()?;
    if partial.values().any(Value::is_null) {
        return None;
    }
    let value: Snapshot = serde_json::from_value(value).ok()?;
    let p = &value.partial;
    (id(&value.id)
        && (1..=MAX_SAFE_INTEGER).contains(&p.seq)
        && p.text.encode_utf16().count() <= 2 * 1024 * 1024
        && p.model.encode_utf16().count() <= 300
        && matches!(p.status.as_str(), "pending" | "received" | "failed")
        && p.segment
            .as_ref()
            .is_none_or(|s| s.encode_utf16().count() <= 310)
        && p.revision.is_none_or(|r| r <= MAX_SAFE_INTEGER))
    .then_some(value)
}

// Mirrors app/shared/turn-stream.ts. Snapshots replace text and a resumed
// provider call supersedes its predecessor even if its text is shorter.
pub fn newest(current: Option<Partial>, incoming: Partial) -> Partial {
    let Some(current) = current else {
        return incoming;
    };
    let newer = if incoming.seq != current.seq {
        incoming.seq > current.seq
    } else if incoming.status != current.status {
        current.status == "pending"
    } else if incoming.revision.is_some() || current.revision.is_some() {
        incoming.revision.unwrap_or(0) > current.revision.unwrap_or(0)
    } else {
        incoming.text.encode_utf16().count() >= current.text.encode_utf16().count()
    };
    if newer {
        incoming
    } else {
        current
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplyContext {
    pub render_id: String,
    pub data: String,
}

impl ReplyContext {
    pub fn valid(&self) -> bool {
        command_id(&self.render_id)
            && self.render_id == self.render_id.to_ascii_lowercase()
            && self.data.len() <= 4096
            && serde_json::from_str::<Value>(&self.data).is_ok()
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Command {
    pub id: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipient: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automatic: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatch_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_turn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html_context: Option<ReplyContext>,
}

impl Command {
    pub fn valid(&self, channel: bool) -> bool {
        command_id(&self.id)
            && self.text.encode_utf16().count() <= MAX_MESSAGE
            && (!self.text.trim().is_empty()
                || self.files.as_ref().is_some_and(|files| !files.is_empty()))
            && self.files.as_ref().is_none_or(|files| {
                files.len() <= MAX_FILES && files.iter().all(|id| command_id(id))
            })
            && self
                .dispatch_mode
                .as_deref()
                .is_none_or(|mode| matches!(mode, "queue" | "steer" | "restart"))
            && self.target_turn.as_deref().is_none_or(|id| {
                command_id(id) && matches!(self.dispatch_mode.as_deref(), Some("steer" | "restart"))
            })
            && self.html_context.as_ref().is_none_or(|context| {
                context.valid()
                    && self
                        .dispatch_mode
                        .as_deref()
                        .is_none_or(|mode| mode == "queue")
            })
            && if channel {
                self.recipient.as_deref().is_none_or(command_id)
                    && self
                        .automatic
                        .is_none_or(|automatic| automatic && self.recipient.is_none())
            } else {
                self.recipient.is_none() && self.automatic.is_none()
            }
    }
}

pub fn restore(raw: &str, channel: bool) -> Option<Command> {
    let value: Value = serde_json::from_str(raw).ok()?;
    if !value.as_object()?.values().all(|value| !value.is_null()) {
        return None;
    }
    let command: Command = serde_json::from_value(value).ok()?;
    command.valid(channel).then_some(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_and_resume() -> Result<(), Box<dyn std::error::Error>> {
        let raw = r#"{"id":"00000000-0000-4000-8000-000000000001","text":"Guide","dispatchMode":"steer","targetTurn":"00000000-0000-4000-8000-000000000002","recipient":"00000000-0000-4000-8000-000000000003"}"#;
        let command = restore(raw, true).ok_or("command rejected")?;
        assert_eq!(
            serde_json::to_value(command)?,
            serde_json::from_str::<Value>(raw)?
        );
        assert!(restore(raw, false).is_none());

        let frame = |seq, revision, text: &str, status: &str, final_frame| Partial {
            seq,
            revision: Some(revision),
            text: text.into(),
            status: status.into(),
            model: "fixture".into(),
            segment: Some("text-0".into()),
            final_frame: Some(final_frame),
        };
        let initial = frame(1, 2, "old call", "pending", false);
        let resumed = frame(2, 1, "new", "pending", false);
        let final_frame = frame(2, 2, "revised", "received", true);
        assert_eq!(newest(Some(initial.clone()), resumed.clone()), resumed);
        assert_eq!(
            newest(Some(resumed.clone()), final_frame.clone()),
            final_frame
        );
        assert_eq!(newest(Some(final_frame.clone()), resumed), final_frame);
        assert_eq!(newest(Some(final_frame.clone()), initial), final_frame);
        Ok(())
    }
}
