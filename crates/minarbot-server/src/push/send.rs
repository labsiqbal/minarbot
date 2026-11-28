//! APNs (token auth over HTTP/2) and FCM HTTP v1 senders.

use super::config::{Apns, Fcm, Push};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{json, Value};
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

/// Apple accepts a provider token for an hour and refuses refreshes more
/// often than every 20 minutes.
const APNS_TOKEN_LIFETIME: Duration = Duration::from_secs(50 * 60);
const FCM_SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
const TIMEOUT: Duration = Duration::from_secs(15);

/// One device the payload goes to.
pub struct Target<'a> {
    pub platform: &'a str,
    pub token: &'a str,
    pub app_id: &'a str,
    pub development: bool,
}

/// What the push service said about one delivery.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Sent,
    /// The token is no longer registered: remove the device.
    Gone,
    /// Transient: try again later.
    Retry(String),
    /// Permanent for this payload: give up and record why.
    Failed(String),
    NotConfigured,
}

struct Cached {
    key: String,
    value: String,
    until: Instant,
}

pub struct Sender {
    apns_client: reqwest::Client,
    client: reqwest::Client,
    apns_token: Mutex<Option<Cached>>,
    fcm_token: Mutex<Option<Cached>>,
}

fn now_seconds() -> i64 {
    crate::config::now() / 1000
}

fn cached(slot: &Mutex<Option<Cached>>, key: &str) -> Option<String> {
    let slot = slot.lock().ok()?;
    slot.as_ref()
        .filter(|item| item.key == key && item.until > Instant::now())
        .map(|item| item.value.clone())
}

fn store(slot: &Mutex<Option<Cached>>, key: &str, value: &str, lifetime: Duration) {
    if let Ok(mut slot) = slot.lock() {
        *slot = Some(Cached {
            key: key.to_owned(),
            value: value.to_owned(),
            until: Instant::now() + lifetime,
        });
    }
}

fn forget(slot: &Mutex<Option<Cached>>) {
    if let Ok(mut slot) = slot.lock() {
        *slot = None;
    }
}

/// ES256 provider token: header `alg`/`kid`, claims `iss` (team) and `iat`.
pub fn apns_jwt(config: &Apns, issued_at: i64) -> Result<String, String> {
    let mut header = Header::new(Algorithm::ES256);
    header.typ = None;
    header.kid = Some(config.key_id.clone());
    let key = EncodingKey::from_ec_pem(config.key.as_bytes())
        .map_err(|_| "The APNs key is invalid.".to_owned())?;
    jsonwebtoken::encode(
        &header,
        &json!({"iss": config.team_id, "iat": issued_at}),
        &key,
    )
    .map_err(|_| "The APNs token could not be signed.".to_owned())
}

/// RS256 service-account assertion for the OAuth2 JWT bearer grant.
pub fn fcm_assertion(config: &Fcm, issued_at: i64) -> Result<String, String> {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = config.private_key_id.clone();
    let key = EncodingKey::from_rsa_pem(config.private_key.as_bytes())
        .map_err(|_| "The FCM key is invalid.".to_owned())?;
    jsonwebtoken::encode(
        &header,
        &json!({"iss": config.client_email, "scope": FCM_SCOPE, "aud": config.token_url,
            "iat": issued_at, "exp": issued_at + 3600}),
        &key,
    )
    .map_err(|_| "The FCM token could not be signed.".to_owned())
}

/// The APNs request body: alert plus the ids that open the right screen.
pub fn apns_body(payload: &Value) -> Value {
    let mut body = json!({"aps": {"alert": {"title": payload["title"], "body": payload["body"]},
        "sound": "default"}});
    if let Some(thread) = payload["group"].as_str() {
        body["aps"]["thread-id"] = json!(thread);
    }
    if let Some(data) = payload["data"].as_object() {
        for (key, value) in data {
            body[key] = value.clone();
        }
    }
    body
}

/// The FCM v1 `messages:send` body.
pub fn fcm_body(payload: &Value, token: &str) -> Value {
    let mut android = json!({"priority": "high"});
    if let Some(tag) = payload["tag"].as_str() {
        android["notification"] = json!({"tag": tag});
    }
    json!({"message": {"token": token,
        "notification": {"title": payload["title"], "body": payload["body"]},
        "data": payload["data"], "android": android}})
}

/// Classifies an APNs answer; `reason` comes from the JSON body.
pub fn apns_outcome(status: u16, reason: &str) -> Outcome {
    match (status, reason) {
        (200, _) => Outcome::Sent,
        (410, _) | (400, "BadDeviceToken") | (_, "Unregistered") => Outcome::Gone,
        (403, "ExpiredProviderToken" | "InvalidProviderToken") => {
            Outcome::Retry(format!("APNs answered 403 ({reason})."))
        }
        (429, _) | (500..=599, _) => Outcome::Retry(format!("APNs answered {status}.")),
        _ if reason.is_empty() => Outcome::Failed(format!("APNs answered {status}.")),
        _ => Outcome::Failed(format!("APNs answered {status} ({reason}).")),
    }
}

/// Classifies an FCM answer from its status and `error` object.
pub fn fcm_outcome(status: u16, error: &Value) -> Outcome {
    let code = error["status"].as_str().unwrap_or("");
    let unregistered = error["details"].as_array().is_some_and(|details| {
        details
            .iter()
            .any(|detail| detail["errorCode"] == "UNREGISTERED")
    });
    match status {
        200 => Outcome::Sent,
        // Only FCM's own error object removes a device: a bare 404 from a
        // wrong URL or proxy must not wipe every registration.
        _ if unregistered || code == "NOT_FOUND" || code == "UNREGISTERED" => Outcome::Gone,
        401 | 429 | 500..=599 => Outcome::Retry(format!("FCM answered {status}.")),
        _ if code.is_empty() => Outcome::Failed(format!("FCM answered {status}.")),
        _ => Outcome::Failed(format!("FCM answered {status} ({code}).")),
    }
}

fn safe(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(64)
        .collect()
}

impl Sender {
    pub fn new() -> Self {
        let build = |h2: bool| {
            let builder = reqwest::Client::builder()
                .timeout(TIMEOUT)
                .redirect(reqwest::redirect::Policy::none());
            let builder = if h2 {
                builder.http2_prior_knowledge()
            } else {
                builder
            };
            builder.build().unwrap_or_default()
        };
        Self {
            apns_client: build(true),
            client: build(false),
            apns_token: Mutex::new(None),
            fcm_token: Mutex::new(None),
        }
    }

    pub async fn send(&self, config: &Push, target: &Target<'_>, payload: &Value) -> Outcome {
        match target.platform {
            "ios" => match &config.apns {
                Some(apns) => self.apns(apns, target, payload).await,
                None => Outcome::NotConfigured,
            },
            "android" => match &config.fcm {
                Some(fcm) => self.fcm(fcm, target.token, payload).await,
                None => Outcome::NotConfigured,
            },
            _ => Outcome::Failed("Unknown platform.".into()),
        }
    }

    fn apns_token(&self, config: &Apns) -> Result<String, String> {
        let key = format!(
            "{}:{}:{}",
            config.team_id,
            config.key_id,
            digest(&config.key)
        );
        if let Some(token) = cached(&self.apns_token, &key) {
            return Ok(token);
        }
        let token = apns_jwt(config, now_seconds())?;
        store(&self.apns_token, &key, &token, APNS_TOKEN_LIFETIME);
        Ok(token)
    }

    async fn apns(&self, config: &Apns, target: &Target<'_>, payload: &Value) -> Outcome {
        let jwt = match self.apns_token(config) {
            Ok(jwt) => jwt,
            Err(message) => return Outcome::Failed(message),
        };
        let url = format!(
            "{}/3/device/{}",
            config.host(target.development),
            target.token
        );
        let response = self
            .apns_client
            .post(url)
            .header("authorization", format!("bearer {jwt}"))
            .header("apns-topic", target.app_id)
            .header("apns-push-type", "alert")
            .header("apns-priority", "10")
            .json(&apns_body(payload))
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(_) => return Outcome::Retry("APNs could not be reached.".into()),
        };
        let status = response.status().as_u16();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        let reason = safe(body["reason"].as_str().unwrap_or(""));
        let outcome = apns_outcome(status, &reason);
        if status == 403 {
            forget(&self.apns_token);
        }
        outcome
    }

    async fn fcm_token(&self, config: &Fcm) -> Result<String, Outcome> {
        let key = format!(
            "{}:{}:{}",
            config.client_email,
            config.token_url,
            digest(&config.private_key)
        );
        if let Some(token) = cached(&self.fcm_token, &key) {
            return Ok(token);
        }
        let assertion = fcm_assertion(config, now_seconds()).map_err(Outcome::Failed)?;
        let response = self
            .client
            .post(&config.token_url)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await
            .map_err(|_| Outcome::Retry("The FCM token service could not be reached.".into()))?;
        let status = response.status().as_u16();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        let token = body["access_token"].as_str().filter(|_| status == 200);
        let Some(token) = token else {
            let message = format!("The FCM token service answered {status}.");
            return Err(if status == 429 || status >= 500 {
                Outcome::Retry(message)
            } else {
                Outcome::Failed(message)
            });
        };
        // Refresh a minute early; Google issues tokens for an hour.
        let lifetime = body["expires_in"].as_u64().unwrap_or(3600).clamp(60, 3600) - 60;
        store(&self.fcm_token, &key, token, Duration::from_secs(lifetime));
        Ok(token.to_owned())
    }

    async fn fcm(&self, config: &Fcm, token: &str, payload: &Value) -> Outcome {
        let access = match self.fcm_token(config).await {
            Ok(access) => access,
            Err(outcome) => return outcome,
        };
        let url = format!(
            "{}/v1/projects/{}/messages:send",
            config.url, config.project_id
        );
        let response = self
            .client
            .post(url)
            .bearer_auth(access)
            .json(&fcm_body(payload, token))
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(_) => return Outcome::Retry("FCM could not be reached.".into()),
        };
        let status = response.status().as_u16();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if status == 401 {
            forget(&self.fcm_token);
        }
        let mut error = body["error"].clone();
        if let Some(code) = error["status"].as_str() {
            error["status"] = json!(safe(code));
        }
        fcm_outcome(status, &error)
    }
}

fn digest(text: &str) -> String {
    crate::data::digest_bytes(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{decode, decode_header, DecodingKey, Validation};

    const APNS_KEY: &str = include_str!("../../../minarbot-testkit/fixtures/push-apns-test-key.p8");
    const APNS_PUBLIC: &str =
        include_str!("../../../minarbot-testkit/fixtures/push-apns-test-key.pub.pem");
    const FCM_KEY: &str = include_str!("../../../minarbot-testkit/fixtures/push-fcm-test-key.pem");
    const FCM_PUBLIC: &str =
        include_str!("../../../minarbot-testkit/fixtures/push-fcm-test-key.pub.pem");
    type Result = std::result::Result<(), Box<dyn std::error::Error>>;

    fn apns() -> Apns {
        Apns {
            team_id: "TEAM123456".into(),
            key_id: "KEY1234567".into(),
            key: APNS_KEY.into(),
            url: None,
        }
    }

    fn fcm() -> Fcm {
        Fcm {
            project_id: "minarbot-test".into(),
            client_email: "push@minarbot-test.iam.gserviceaccount.com".into(),
            private_key: FCM_KEY.into(),
            private_key_id: Some("key-1".into()),
            url: super::super::config::FCM.into(),
            token_url: super::super::config::FCM_TOKEN.into(),
        }
    }

    #[test]
    fn apns_jwt_has_es256_header_kid_and_team_claims() -> Result {
        let token = apns_jwt(&apns(), 1_700_000_000)?;
        let header = decode_header(&token)?;
        assert_eq!(header.alg, Algorithm::ES256);
        assert_eq!(header.kid.as_deref(), Some("KEY1234567"));
        assert_eq!(header.typ, None);
        let mut validation = Validation::new(Algorithm::ES256);
        validation.required_spec_claims.clear();
        validation.validate_exp = false;
        let data = decode::<Value>(
            &token,
            &DecodingKey::from_ec_pem(APNS_PUBLIC.as_bytes())?,
            &validation,
        )?;
        assert_eq!(
            data.claims,
            json!({"iss": "TEAM123456", "iat": 1_700_000_000})
        );
        Ok(())
    }

    #[test]
    fn fcm_assertion_is_rs256_for_the_messaging_scope() -> Result {
        let token = fcm_assertion(&fcm(), 1_700_000_000)?;
        let header = decode_header(&token)?;
        assert_eq!(header.alg, Algorithm::RS256);
        assert_eq!(header.kid.as_deref(), Some("key-1"));
        let mut validation = Validation::new(Algorithm::RS256);
        validation.validate_exp = false;
        validation.set_audience(&["https://oauth2.googleapis.com/token"]);
        let data = decode::<Value>(
            &token,
            &DecodingKey::from_rsa_pem(FCM_PUBLIC.as_bytes())?,
            &validation,
        )?;
        assert_eq!(
            data.claims,
            json!({"iss": "push@minarbot-test.iam.gserviceaccount.com",
                "scope": "https://www.googleapis.com/auth/firebase.messaging",
                "aud": "https://oauth2.googleapis.com/token",
                "iat": 1_700_000_000, "exp": 1_700_003_600})
        );
        Ok(())
    }

    #[test]
    fn apns_token_is_cached_per_key_and_refreshed_after_forgetting() -> Result {
        let sender = Sender::new();
        let first = sender.apns_token(&apns())?;
        assert_eq!(sender.apns_token(&apns())?, first);
        let mut other = apns();
        other.key_id = "KEY7654321".into();
        let second = sender.apns_token(&other)?;
        assert_ne!(second, first);
        assert_eq!(decode_header(&second)?.kid.as_deref(), Some("KEY7654321"));
        forget(&sender.apns_token);
        assert!(cached(&sender.apns_token, "anything").is_none());
        store(&sender.fcm_token, "k", "v", Duration::from_secs(0));
        assert!(cached(&sender.fcm_token, "k").is_none(), "expired token");
        store(&sender.fcm_token, "k", "v", Duration::from_secs(60));
        assert_eq!(cached(&sender.fcm_token, "k").as_deref(), Some("v"));
        assert!(cached(&sender.fcm_token, "other").is_none());
        Ok(())
    }

    #[test]
    fn payload_shapes() {
        let payload = json!({"title": "Ada", "body": "Finished", "group": "thread-1",
            "tag": "abc", "data": {"kind": "finished", "agent": "a", "path": "/agents/a"}});
        assert_eq!(
            apns_body(&payload),
            json!({"aps": {"alert": {"title": "Ada", "body": "Finished"}, "sound": "default",
                "thread-id": "thread-1"}, "kind": "finished", "agent": "a", "path": "/agents/a"})
        );
        assert_eq!(
            fcm_body(&payload, "tok"),
            json!({"message": {"token": "tok", "notification": {"title": "Ada", "body": "Finished"},
                "data": {"kind": "finished", "agent": "a", "path": "/agents/a"},
                "android": {"priority": "high", "notification": {"tag": "abc"}}}})
        );
    }

    #[test]
    fn error_classification() {
        assert_eq!(apns_outcome(200, ""), Outcome::Sent);
        assert_eq!(apns_outcome(410, "Unregistered"), Outcome::Gone);
        assert_eq!(apns_outcome(400, "BadDeviceToken"), Outcome::Gone);
        assert!(matches!(apns_outcome(503, ""), Outcome::Retry(_)));
        assert!(matches!(
            apns_outcome(429, "TooManyRequests"),
            Outcome::Retry(_)
        ));
        assert!(matches!(
            apns_outcome(403, "ExpiredProviderToken"),
            Outcome::Retry(_)
        ));
        assert_eq!(
            apns_outcome(400, "BadTopic"),
            Outcome::Failed("APNs answered 400 (BadTopic).".into())
        );
        assert_eq!(fcm_outcome(200, &Value::Null), Outcome::Sent);
        assert_eq!(
            fcm_outcome(404, &json!({"status": "NOT_FOUND"})),
            Outcome::Gone
        );
        assert_eq!(
            fcm_outcome(
                400,
                &json!({"status": "INVALID_ARGUMENT",
                "details": [{"@type": "type.googleapis.com/google.firebase.fcm.v1.FcmError",
                    "errorCode": "UNREGISTERED"}]})
            ),
            Outcome::Gone
        );
        assert!(matches!(
            fcm_outcome(503, &json!({"status": "UNAVAILABLE"})),
            Outcome::Retry(_)
        ));
        assert!(matches!(fcm_outcome(401, &Value::Null), Outcome::Retry(_)));
        assert!(matches!(fcm_outcome(404, &Value::Null), Outcome::Failed(_)));
        assert_eq!(
            fcm_outcome(400, &json!({"status": "INVALID_ARGUMENT"})),
            Outcome::Failed("FCM answered 400 (INVALID_ARGUMENT).".into())
        );
    }
}
