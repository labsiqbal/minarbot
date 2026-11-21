use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use serde::Deserialize;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CanonicalBridge {
    pub origin: String,
    pub token: String,
    pub clerk: bool,
    pub mail: bool,
    pub webhook: bool,
}
impl CanonicalBridge {
    pub(crate) fn call(&self, operation: &str, value: &Value) -> Result<Value> {
        use std::io::{Read, Write};
        let url =
            crate::connectors::config::loopback(&self.origin).map_err(|_| Failure::storage())?;
        let address =
            std::net::SocketAddr::from(([127, 0, 0, 1], url.port().ok_or_else(Failure::storage)?));
        let mut socket =
            std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_secs(5))
                .map_err(|_| Failure::storage())?;
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .map_err(|_| Failure::storage())?;
        socket
            .set_write_timeout(Some(std::time::Duration::from_secs(5)))
            .map_err(|_| Failure::storage())?;
        let body = value.to_string();
        write!(socket, "POST /{operation} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",self.origin.trim_start_matches("http://"),self.token,body.len(),body).map_err(|_|Failure::storage())?;
        let mut raw = String::new();
        socket
            .take(65537)
            .read_to_string(&mut raw)
            .map_err(|_| Failure::storage())?;
        if raw.len() > 65536 || !raw.starts_with("HTTP/1.1 200 ") {
            return Err(Failure::storage());
        }
        let (_, body) = raw.split_once("\r\n\r\n").ok_or_else(Failure::storage)?;
        serde_json::from_str::<Value>(body)
            .map_err(|_| Failure::storage())?
            .get("value")
            .cloned()
            .ok_or_else(Failure::storage)
    }
}

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Options {
    pub test_canonical_bridge: Option<CanonicalBridge>,
    pub script_token: Option<String>,
    pub installation_id: Option<String>,
    pub mail_from: Option<String>,
    pub mail_origin: Option<String>,
    pub test_keyring: Option<HashMap<String, String>>,
    #[serde(default)]
    pub test_keyring_locked: bool,
    #[serde(skip)]
    stored: Arc<Mutex<HashMap<String, String>>>,
}
pub(crate) fn unavailable(mail: bool) -> Failure {
    let mut error = if mail {
        Failure::new(
            503,
            "mail_key_unavailable",
            "Mail key is not available yet.",
        )
    } else {
        Failure::new(
            503,
            "keystore_unavailable",
            "Sync key is not configured yet.",
        )
    };
    error.retryable = true;
    error
}
impl Options {
    fn service(&self, name: &str, mail: bool) -> Result<String> {
        if name.starts_with("vault-aead/") {
            return Ok("minarbot-vault".into());
        }
        let installation = self
            .installation_id
            .as_ref()
            .ok_or_else(|| unavailable(mail))?;
        Ok(format!(
            "minarbot/server/{installation}{}",
            if name.starts_with("routine-webhook/") {
                "/routine-webhooks"
            } else {
                ""
            }
        ))
    }
    pub fn environment() -> Self {
        Self {
            script_token: std::env::var("MINARBOT_SCRIPT_TOKEN").ok(),
            installation_id: std::env::var("MINARBOT_INSTALLATION_ID").ok(),
            mail_from: std::env::var("MINARBOT_MAIL_FROM").ok(),
            mail_origin: std::env::var("MINARBOT_ORIGIN").ok(),
            ..Self::default()
        }
    }
    pub fn validate(&self) -> crate::db::Result<()> {
        if let Some(bridge) = &self.test_canonical_bridge {
            if std::env::var("MINARBOT_TEST_SERVER").as_deref() != Ok("rust") {
                return Err("Canonical fixture requires Rust test mode".into());
            }
            crate::connectors::config::loopback(&bridge.origin)?;
            if bridge.token.len() != 64 || !bridge.token.bytes().all(|c| c.is_ascii_hexdigit()) {
                return Err("Invalid canonical fixture token".into());
            }
        }
        if self.script_token.as_ref().is_some_and(|s| {
            !(32..=256).contains(&s.len())
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        }) {
            return Err("MINARBOT_SCRIPT_TOKEN must be 32..256 URL-safe characters".into());
        }
        if self.installation_id.as_ref().is_some_and(|s| !uuid(s)) {
            return Err("Server installation ID must be UUID v4".into());
        }
        Ok(())
    }
    pub(crate) fn read(&self, name: &str, mail: bool) -> Result<Option<String>> {
        if let Some(bridge) = &self.test_canonical_bridge {
            if bridge.mail && name.starts_with("mail-") {
                return bridge
                    .call("mail-key", &json!({"name":name}))
                    .map(|v| v.as_str().map(str::to_owned))
                    .map_err(|_| unavailable(true));
            }
        }
        if self.test_keyring_locked {
            return Err(unavailable(mail));
        }
        if let Some(stub) = &self.test_keyring {
            let saved = self.stored.lock().map_err(|_| unavailable(mail))?;
            return Ok(saved.get(name).or_else(|| stub.get(name)).cloned());
        }
        if std::env::var("MINARBOT_TEST_SERVER").as_deref() == Ok("rust") {
            return Err(unavailable(mail));
        }
        crate::system_keyring::read(&self.service(name, mail)?, name).map_err(|_| unavailable(mail))
    }
    pub(crate) fn write(&self, name: &str, value: &str, mail: bool) -> Result<()> {
        if self.test_keyring_locked {
            return Err(unavailable(mail));
        }
        if self.test_keyring.is_some() {
            self.stored
                .lock()
                .map_err(|_| unavailable(mail))?
                .insert(name.into(), value.into());
            return Ok(());
        }
        if std::env::var("MINARBOT_TEST_SERVER").as_deref() == Ok("rust") {
            return Err(unavailable(mail));
        }
        crate::system_keyring::write(&self.service(name, mail)?, name, value)
            .map_err(|_| unavailable(mail))
    }
    pub(crate) fn key(&self, name: &str, mail: bool) -> Result<Vec<u8>> {
        let encoded = self.read(name, mail)?.ok_or_else(|| unavailable(mail))?;
        B64.decode(&encoded)
            .ok()
            .filter(|b| b.len() == 32 && B64.encode(b) == encoded)
            .ok_or_else(|| unavailable(mail))
    }
    pub(super) fn sync_key(&self) -> Result<Vec<u8>> {
        let name = "sync-hmac/v1";
        ensure_sync_key(
            || self.read(name, false),
            |encoded| self.write(name, encoded, false),
        )?;
        self.key(name, false)
    }
    pub(super) fn mail(&self) -> Result<(String, Vec<u8>, Vec<u8>)> {
        let id = self
            .read("mail-aead/active", true)?
            .filter(|s| uuid(s))
            .ok_or_else(|| unavailable(true))?;
        Ok((
            id.clone(),
            self.key(&format!("mail-aead/{id}"), true)?,
            self.key("mail-lookup/v1", true)?,
        ))
    }
}

fn ensure_sync_key(
    read: impl Fn() -> Result<Option<String>>,
    write: impl FnOnce(&str) -> Result<()>,
) -> Result<()> {
    if read()?.is_none() {
        let mut key = [0; 32];
        getrandom::getrandom(&mut key).map_err(|_| unavailable(false))?;
        let encoded = B64.encode(key);
        write(&encoded)?;
        if read()?.as_deref() != Some(&encoded) {
            return Err(unavailable(false));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_keyring_retains_one_verified_key_and_rejects_unavailable_storage() -> Result<()> {
        let installation = uuid::Uuid::new_v4().to_string();
        let mut options = Options {
            installation_id: Some(installation.clone()),
            test_keyring: Some(HashMap::new()),
            ..Options::default()
        };
        let name = "sync-hmac/v1";
        assert_eq!(
            options.service(name, false)?,
            format!("minarbot/server/{installation}")
        );
        let key = options.sync_key()?;
        assert_eq!(key.len(), 32);
        assert_eq!(options.sync_key()?, key);
        let stored = options.stored.lock().map_err(|_| unavailable(false))?;
        assert_eq!(stored.len(), 1);
        assert_eq!(stored.get(name), Some(&B64.encode(&key)));
        drop(stored);
        options.write(name, "corrupt", false)?;
        assert_eq!(
            options.sync_key().err().map(|error| error.code),
            Some("keystore_unavailable")
        );
        options.test_keyring_locked = true;
        let writes = std::cell::Cell::new(0);
        let locked = ensure_sync_key(
            || options.read(name, false),
            |_| {
                writes.set(writes.get() + 1);
                Ok(())
            },
        );
        assert_eq!(
            locked.err().map(|error| error.code),
            Some("keystore_unavailable")
        );
        assert_eq!(writes.get(), 0);
        let lost_write = ensure_sync_key(|| Ok(None), |_| Ok(()));
        assert_eq!(
            lost_write.err().map(|error| error.code),
            Some("keystore_unavailable")
        );
        Ok(())
    }
}
