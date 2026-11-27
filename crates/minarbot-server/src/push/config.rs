//! Push credentials from the owner's machine, never from the repository.
//!
//! Files live in `<data directory>/push` (`MINARBOT_PUSH_DIR` overrides it):
//! - `apns.json`: `{"teamId": "...", "keyId": "...", "keyFile": "AuthKey_X.p8"}`
//!   (`keyFile` is relative to the push directory and defaults to `apns.p8`);
//! - `fcm.json`: the Firebase service-account JSON as downloaded.
//!
//! They are read on every use, so adding or replacing them needs no restart.
//! `MINARBOT_APNS_URL`, `MINARBOT_FCM_URL` and `MINARBOT_FCM_TOKEN_URL`
//! override the service addresses (tests point them at a fake).

use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const APNS_PRODUCTION: &str = "https://api.push.apple.com";
pub const APNS_DEVELOPMENT: &str = "https://api.sandbox.push.apple.com";
pub const FCM: &str = "https://fcm.googleapis.com";
pub const FCM_TOKEN: &str = "https://oauth2.googleapis.com/token";
const MAX_FILE: u64 = 64 * 1024;

#[derive(Clone)]
pub struct Apns {
    pub team_id: String,
    pub key_id: String,
    pub key: String,
    /// Replaces both the production and the sandbox host.
    pub url: Option<String>,
}

impl Apns {
    pub fn host(&self, development: bool) -> &str {
        match &self.url {
            Some(url) => url,
            None if development => APNS_DEVELOPMENT,
            None => APNS_PRODUCTION,
        }
    }
}

#[derive(Clone)]
pub struct Fcm {
    pub project_id: String,
    pub client_email: String,
    pub private_key: String,
    pub private_key_id: Option<String>,
    pub url: String,
    pub token_url: String,
}

#[derive(Clone, Default)]
pub struct Push {
    pub apns: Option<Apns>,
    pub fcm: Option<Fcm>,
}

impl Push {
    pub fn configured(&self, platform: &str) -> bool {
        match platform {
            "ios" => self.apns.is_some(),
            "android" => self.fcm.is_some(),
            _ => false,
        }
    }
}

pub fn directory(database: &Path) -> PathBuf {
    std::env::var_os("MINARBOT_PUSH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            database
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("push")
        })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ApnsFile {
    team_id: String,
    key_id: String,
    key_file: Option<String>,
}

#[derive(Deserialize)]
struct ServiceAccount {
    project_id: String,
    client_email: String,
    private_key: String,
    private_key_id: Option<String>,
}

fn read(path: &Path) -> Result<Option<String>, String> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(format!("{name} cannot be read")),
        Ok(meta) if !meta.is_file() || meta.len() > MAX_FILE => {
            return Err(format!("{name} is not a small regular file"))
        }
        Ok(_) => (),
    }
    std::fs::read_to_string(path)
        .map(Some)
        .map_err(|_| format!("{name} cannot be read"))
}

fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64 && value.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn apns(directory: &Path) -> Result<Option<Apns>, String> {
    let Some(raw) = read(&directory.join("apns.json"))? else {
        return Ok(None);
    };
    let file: ApnsFile =
        serde_json::from_str(&raw).map_err(|_| "apns.json is invalid".to_owned())?;
    if !identifier(&file.team_id) || !identifier(&file.key_id) {
        return Err("apns.json needs teamId and keyId".into());
    }
    let key_file = directory.join(file.key_file.as_deref().unwrap_or("apns.p8"));
    let key = read(&key_file)?.ok_or_else(|| "the APNs key file is missing".to_owned())?;
    jsonwebtoken::EncodingKey::from_ec_pem(key.as_bytes())
        .map_err(|_| "the APNs key is not a PKCS#8 P-256 key".to_owned())?;
    Ok(Some(Apns {
        team_id: file.team_id,
        key_id: file.key_id,
        key,
        url: override_url("MINARBOT_APNS_URL"),
    }))
}

fn fcm(directory: &Path) -> Result<Option<Fcm>, String> {
    let Some(raw) = read(&directory.join("fcm.json"))? else {
        return Ok(None);
    };
    let account: ServiceAccount =
        serde_json::from_str(&raw).map_err(|_| "fcm.json is invalid".to_owned())?;
    let project = !account.project_id.is_empty()
        && account.project_id.len() <= 64
        && account
            .project_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if !project || account.client_email.is_empty() {
        return Err("fcm.json needs project_id and client_email".into());
    }
    jsonwebtoken::EncodingKey::from_rsa_pem(account.private_key.as_bytes())
        .map_err(|_| "fcm.json has no usable private_key".to_owned())?;
    Ok(Some(Fcm {
        project_id: account.project_id,
        client_email: account.client_email,
        private_key: account.private_key,
        private_key_id: account.private_key_id.filter(|id| !id.is_empty()),
        url: override_url("MINARBOT_FCM_URL").unwrap_or_else(|| FCM.to_owned()),
        token_url: override_url("MINARBOT_FCM_TOKEN_URL").unwrap_or_else(|| FCM_TOKEN.to_owned()),
    }))
}

fn override_url(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|url| url.trim_end_matches('/').to_owned())
        .filter(|url| url.starts_with("https://") || url.starts_with("http://"))
}

/// Loads both services; an invalid file counts as not configured and is
/// reported once per distinct problem (names only, never contents).
pub fn load(database: &Path) -> Push {
    let directory = directory(database);
    let apns = apns(&directory).unwrap_or_else(|problem| {
        warn(&problem);
        None
    });
    let fcm = fcm(&directory).unwrap_or_else(|problem| {
        warn(&problem);
        None
    });
    Push { apns, fcm }
}

fn warn(problem: &str) {
    static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    if let Ok(mut seen) = SEEN.lock() {
        if !seen.iter().any(|item| item == problem) {
            seen.push(problem.to_owned());
            eprintln!("Push is not configured: {problem}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APNS_KEY: &str = include_str!("../../../minarbot-testkit/fixtures/push-apns-test-key.p8");
    const FCM_KEY: &str = include_str!("../../../minarbot-testkit/fixtures/push-fcm-test-key.pem");

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_invalid_and_valid_files() -> Result<(), Box<dyn std::error::Error>> {
        let dir = Dir(std::env::temp_dir().join(format!("minarbot-push-{}", uuid::Uuid::new_v4())));
        std::fs::create_dir_all(&dir.0)?;
        assert!(apns(&dir.0)?.is_none() && fcm(&dir.0)?.is_none());
        std::fs::write(
            dir.0.join("apns.json"),
            r#"{"teamId":"TEAM123456","keyId":"KEY1234567"}"#,
        )?;
        assert_eq!(
            apns(&dir.0).err().as_deref(),
            Some("the APNs key file is missing")
        );
        std::fs::write(dir.0.join("apns.p8"), "not a key")?;
        assert!(apns(&dir.0).is_err());
        std::fs::write(dir.0.join("apns.p8"), APNS_KEY)?;
        let config = apns(&dir.0)?.ok_or("apns")?;
        assert_eq!(
            (config.team_id.as_str(), config.key_id.as_str()),
            ("TEAM123456", "KEY1234567")
        );
        std::fs::write(
            dir.0.join("apns.json"),
            r#"{"teamId":"TEAM 1","keyId":"KEY1234567"}"#,
        )?;
        assert!(apns(&dir.0).is_err());
        std::fs::write(dir.0.join("fcm.json"), "{}")?;
        assert!(fcm(&dir.0).is_err());
        let account = serde_json::json!({"type": "service_account", "project_id": "minarbot-test",
            "private_key_id": "abc", "private_key": FCM_KEY, "client_email": "push@minarbot-test.iam.gserviceaccount.com",
            "token_uri": "https://oauth2.googleapis.com/token"});
        std::fs::write(dir.0.join("fcm.json"), account.to_string())?;
        let config = fcm(&dir.0)?.ok_or("fcm")?;
        assert_eq!(config.project_id, "minarbot-test");
        assert_eq!(config.private_key_id.as_deref(), Some("abc"));
        Ok(())
    }
}
