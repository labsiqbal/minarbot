//! Fake APNs and FCM endpoints for push tests.
//!
//! [`FakePush`] serves APNs (`POST /3/device/<token>`, HTTP/2 prior
//! knowledge), the Google OAuth2 token endpoint (`POST /token`) and FCM HTTP
//! v1 (`POST /v1/projects/<project>/messages:send`) on one loopback port and
//! records every request. [`FakePush::configure`] writes test-only
//! credentials into the root's `data/push` directory and points the server
//! at the fake; without it the server has no push configuration.

use crate::{server::ServerConfig, Result, TestRoot};
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri, Version},
    response::{IntoResponse, Response},
    Router,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::{net::TcpListener, task::JoinHandle};

/// Test-only P-256 key standing in for an APNs `.p8` key.
pub const APNS_KEY: &str = include_str!("../fixtures/push-apns-test-key.p8");
pub const APNS_PUBLIC_KEY: &str = include_str!("../fixtures/push-apns-test-key.pub.pem");
/// Test-only RSA key standing in for a Firebase service-account key.
pub const FCM_KEY: &str = include_str!("../fixtures/push-fcm-test-key.pem");
pub const FCM_PUBLIC_KEY: &str = include_str!("../fixtures/push-fcm-test-key.pub.pem");
pub const TEAM_ID: &str = "TEAM123456";
pub const KEY_ID: &str = "KEY1234567";
pub const PROJECT: &str = "minarbot-test";
pub const CLIENT_EMAIL: &str = "push@minarbot-test.iam.gserviceaccount.com";
pub const ACCESS_TOKEN: &str = "fake-fcm-access-token";

/// One recorded request.
#[derive(Clone, Debug)]
pub struct PushRequest {
    pub path: String,
    pub http2: bool,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl PushRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// Form field of an `application/x-www-form-urlencoded` body.
    pub fn form(&self, name: &str) -> Option<String> {
        url_decode_pairs(&self.body)
            .into_iter()
            .find_map(|(key, value)| (key == name).then_some(value))
    }

    /// The APNs device token or the FCM `message.token`.
    pub fn device_token(&self) -> Option<String> {
        if let Some(token) = self.path.strip_prefix("/3/device/") {
            return Some(token.to_owned());
        }
        self.json()["message"]["token"].as_str().map(str::to_owned)
    }
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' => match text
                .get(index + 1..index + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
            {
                Some(byte) => {
                    out.push(byte);
                    index += 3;
                }
                None => {
                    out.push(b'%');
                    index += 1;
                }
            },
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn url_decode_pairs(body: &[u8]) -> Vec<(String, String)> {
    String::from_utf8_lossy(body)
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(key), decode(value))
        })
        .collect()
}

#[derive(Clone, Default)]
struct Shared {
    requests: Arc<Mutex<Vec<PushRequest>>>,
    /// Scripted answers by device token (status and JSON body).
    answers: Arc<Mutex<HashMap<String, (u16, Value)>>>,
}

/// Running fake. Dropping it stops the listener.
pub struct FakePush {
    origin: String,
    shared: Shared,
    task: JoinHandle<()>,
}

impl FakePush {
    pub async fn start() -> Result<Self> {
        let shared = Shared::default();
        let app = Router::new().fallback(handle).with_state(shared.clone());
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self {
            origin,
            shared,
            task,
        })
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Answer pushes to `token` with `status` and `body` instead of 200.
    pub fn answer(&self, token: &str, status: u16, body: Value) {
        if let Ok(mut answers) = self.shared.answers.lock() {
            answers.insert(token.to_owned(), (status, body));
        }
    }

    pub fn requests(&self) -> Vec<PushRequest> {
        match self.shared.requests.lock() {
            Ok(requests) => requests.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Pushes (APNs and FCM sends), without OAuth2 token requests.
    pub fn pushes(&self) -> Vec<PushRequest> {
        self.requests()
            .into_iter()
            .filter(|request| request.path != "/token")
            .collect()
    }

    /// Writes test credentials under `<root>/data/push` and returns `config`
    /// with the service URLs pointed at this fake.
    pub fn configure(&self, root: &TestRoot, config: ServerConfig) -> Result<ServerConfig> {
        let directory = root.dir("data/push")?;
        std::fs::write(
            directory.join("apns.json"),
            json!({"teamId": TEAM_ID, "keyId": KEY_ID}).to_string(),
        )?;
        std::fs::write(directory.join("apns.p8"), APNS_KEY)?;
        std::fs::write(
            directory.join("fcm.json"),
            json!({"type": "service_account", "project_id": PROJECT,
                "private_key_id": "test-key", "private_key": FCM_KEY,
                "client_email": CLIENT_EMAIL, "token_uri": format!("{}/token", self.origin)})
            .to_string(),
        )?;
        Ok(config
            .env("MINARBOT_APNS_URL", &self.origin)
            .env("MINARBOT_FCM_URL", &self.origin)
            .env("MINARBOT_FCM_TOKEN_URL", format!("{}/token", self.origin)))
    }
}

impl Drop for FakePush {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle(
    State(shared): State<Shared>,
    method: Method,
    version: Version,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request = PushRequest {
        path: uri.path().to_owned(),
        http2: version == Version::HTTP_2,
        headers: headers
            .iter()
            .map(|(key, value)| {
                (
                    key.as_str().to_owned(),
                    value.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect(),
        body: body.to_vec(),
    };
    match shared.requests.lock() {
        Ok(mut requests) => requests.push(request.clone()),
        Err(poisoned) => poisoned.into_inner().push(request.clone()),
    }
    let reply = |status: u16, value: Value| {
        (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            [("content-type", "application/json")],
            value.to_string(),
        )
            .into_response()
    };
    if method != Method::POST {
        return reply(405, json!({"error": "POST only"}));
    }
    if request.path == "/token" {
        return reply(
            200,
            json!({"access_token": ACCESS_TOKEN, "expires_in": 3599, "token_type": "Bearer"}),
        );
    }
    let scripted = request.device_token().and_then(|token| {
        shared
            .answers
            .lock()
            .ok()
            .and_then(|answers| answers.get(&token).cloned())
    });
    if let Some((status, body)) = scripted {
        return reply(status, body);
    }
    if request.path.starts_with("/3/device/") {
        return reply(200, json!({}));
    }
    if request.path == format!("/v1/projects/{PROJECT}/messages:send") {
        return reply(
            200,
            json!({"name": format!("projects/{PROJECT}/messages/1")}),
        );
    }
    reply(404, json!({"error": "unknown fake push path"}))
}

/// Verifies the APNs provider token (`authorization: bearer <jwt>`) of a push
/// against the test key: ES256 with `kid` [`KEY_ID`]. Returns its claims.
pub fn apns_claims(request: &PushRequest) -> Result<Value> {
    use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
    let token = request
        .header("authorization")
        .and_then(|value| value.strip_prefix("bearer "))
        .ok_or("APNs push has no bearer provider token")?;
    let header = decode_header(token)?;
    if header.alg != Algorithm::ES256 || header.kid.as_deref() != Some(KEY_ID) {
        return Err(format!("APNs token header: {header:?}").into());
    }
    let mut validation = Validation::new(Algorithm::ES256);
    validation.required_spec_claims.clear();
    validation.validate_exp = false;
    let key = DecodingKey::from_ec_pem(APNS_PUBLIC_KEY.as_bytes())?;
    Ok(decode::<Value>(token, &key, &validation)?.claims)
}

/// Verifies the RS256 service-account assertion of an OAuth2 token request
/// against the test key. Returns its claims.
pub fn fcm_assertion_claims(request: &PushRequest) -> Result<Value> {
    use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
    if request.form("grant_type").as_deref() != Some("urn:ietf:params:oauth:grant-type:jwt-bearer")
    {
        return Err("token request is not a JWT bearer grant".into());
    }
    let assertion = request
        .form("assertion")
        .ok_or("token request has no assertion")?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.validate_aud = false;
    let key = DecodingKey::from_rsa_pem(FCM_PUBLIC_KEY.as_bytes())?;
    Ok(decode::<Value>(&assertion, &key, &validation)?.claims)
}
