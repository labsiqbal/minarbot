//! Command digests matching `app/shared/protocol.ts` and the server.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// JSON with object keys sorted recursively and no whitespace.
pub fn canonical_json(value: &Value) -> String {
    match value {
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(fields) => {
            let mut keys: Vec<&String> = fields.keys().collect();
            keys.sort();
            let body = keys
                .iter()
                .map(|key| {
                    format!(
                        "{}:{}",
                        Value::String((*key).clone()),
                        canonical_json(&fields[key.as_str()])
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{body}}}")
        }
        other => other.to_string(),
    }
}

/// Lowercase hex SHA-256.
pub fn sha256_hex(bytes: impl AsRef<[u8]>) -> String {
    Sha256::digest(bytes.as_ref())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `digestCommand`: SHA-256 of the canonical JSON.
pub fn digest_command(value: &Value) -> String {
    sha256_hex(canonical_json(value))
}
