use super::*;
use reqwest::{header::HeaderMap, Method, RequestBuilder};
use std::{collections::HashMap, sync::Mutex, time::Duration};
use tokio::time::Instant;

#[derive(Default)]
struct Limits {
    global: Option<Instant>,
    routes: HashMap<&'static str, String>,
    buckets: HashMap<(String, String), Instant>,
}

pub(super) struct Api {
    client: reqwest::Client,
    origin: String,
    token: String,
    limits: Mutex<Limits>,
}

fn seconds(value: Option<f64>) -> Option<Duration> {
    value
        .filter(|s| s.is_finite() && *s >= 0.0)
        .map(|s| Duration::from_secs_f64(s.clamp(0.01, 86_400.0)))
}
fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

impl Api {
    pub(super) fn new(state: &AppState, token: String) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .user_agent(concat!(
                    "DiscordBot (https://github.com/labsiqbal/minarbot, ",
                    env!("CARGO_PKG_VERSION"),
                    ")"
                ))
                .build()
                .map_err(|_| Failure::storage())?,
            origin: state
                .config
                .connectors
                .discord_test_origin
                .clone()
                .unwrap_or_else(|| "https://discord.com".into()),
            token,
            limits: Mutex::default(),
        })
    }

    pub(super) async fn request(
        &self,
        route: &'static str,
        major: &str,
        path: &str,
        method: Method,
        build: impl Fn(RequestBuilder) -> Result<RequestBuilder>,
    ) -> Result<Value> {
        let uncertain = || {
            error(
                502,
                "Discord did not confirm the request. The last part may have arrived.",
            )
        };
        // Only a rate-limit rejection is safe to retry. Connection and server errors are ambiguous.
        for attempt in 0..=3 {
            loop {
                let until = {
                    let limits = self.limits.lock().map_err(|_| Failure::storage())?;
                    let bucket = limits.routes.get(route).map_or(route, String::as_str);
                    limits.global.max(
                        limits
                            .buckets
                            .get(&(bucket.to_owned(), major.to_owned()))
                            .copied(),
                    )
                };
                match until {
                    Some(until) if until > Instant::now() => tokio::time::sleep_until(until).await,
                    _ => break,
                }
            }
            let request = self
                .client
                .request(method.clone(), format!("{}/api/v10{path}", self.origin))
                .header("Authorization", format!("Bot {}", self.token))
                .timeout(Duration::from_secs(if route == "interaction" {
                    2
                } else {
                    30
                }));
            let response = build(request)?.send().await.map_err(|_| uncertain())?;
            let status = response.status();
            let headers = response.headers().clone();
            let limited = status.as_u16() == 429;
            let retry = if limited {
                let body: Value = runner::bytes(response, 8192)
                    .await
                    .ok()
                    .and_then(|b| serde_json::from_slice(&b).ok())
                    .unwrap_or(Value::Null);
                let delay = header(&headers, "retry-after")
                    .and_then(|s| s.parse().ok())
                    .or_else(|| body["retry_after"].as_f64());
                Some((
                    seconds(delay).unwrap_or(Duration::from_secs(1)),
                    header(&headers, "x-ratelimit-global") == Some("true")
                        || header(&headers, "x-ratelimit-scope") == Some("global")
                        || body["global"] == true,
                ))
            } else {
                let delay = (header(&headers, "x-ratelimit-remaining") == Some("0")).then(|| {
                    seconds(
                        header(&headers, "x-ratelimit-reset-after").and_then(|s| s.parse().ok()),
                    )
                    .unwrap_or(Duration::from_secs(1))
                });
                self.note(route, major, &headers, delay.map(|d| (d, false)))?;
                return if status.as_u16() == 204 {
                    Ok(Value::Null)
                } else if status.is_success() {
                    serde_json::from_slice(
                        &runner::bytes(response, 512 * 1024)
                            .await
                            .map_err(|_| uncertain())?,
                    )
                    .map_err(|_| uncertain())
                } else if status.is_client_error() {
                    Err(error(
                        status.as_u16(),
                        format!("Discord rejected the request (HTTP {}).", status.as_u16()),
                    ))
                } else {
                    Err(uncertain())
                };
            };
            self.note(route, major, &headers, retry)?;
            if attempt == 3 {
                return Err(error(429, "Discord rate limit prevented delivery."));
            }
        }
        Err(Failure::storage())
    }

    fn note(
        &self,
        route: &'static str,
        major: &str,
        headers: &HeaderMap,
        delay: Option<(Duration, bool)>,
    ) -> Result<()> {
        let mut limits = self.limits.lock().map_err(|_| Failure::storage())?;
        let now = Instant::now();
        limits.buckets.retain(|_, until| *until > now);
        if let Some(bucket) = header(headers, "x-ratelimit-bucket") {
            if let Some(previous) = limits.routes.insert(route, bucket.to_owned()) {
                if previous != bucket {
                    if let Some(until) = limits.buckets.remove(&(previous, major.to_owned())) {
                        limits
                            .buckets
                            .insert((bucket.to_owned(), major.to_owned()), until);
                    }
                }
            }
        }
        if let Some((delay, global)) = delay {
            let until = now + delay;
            if global {
                limits.global = limits.global.max(Some(until));
            } else {
                let bucket = limits
                    .routes
                    .get(route)
                    .map_or(route, String::as_str)
                    .to_owned();
                let current = limits
                    .buckets
                    .entry((bucket, major.to_owned()))
                    .or_insert(until);
                *current = (*current).max(until);
            }
        }
        Ok(())
    }

    pub(super) async fn attachment(&self, url: &str) -> Result<Vec<u8>> {
        let invalid = || error(415, "Discord attachment is unavailable or invalid.");
        let url = url::Url::parse(url).map_err(|_| invalid())?;
        let cdn = url.scheme() == "https"
            && matches!(
                url.host_str(),
                Some("cdn.discordapp.com" | "media.discordapp.net")
            )
            && url.port().is_none()
            && url.path().starts_with("/attachments/");
        let fixture = self.origin.starts_with("http://127.0.0.1:")
            && url.origin().ascii_serialization() == self.origin;
        if !(cdn || fixture) || !url.username().is_empty() || url.password().is_some() {
            return Err(invalid());
        }
        let response = self
            .client
            .get(url)
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|_| invalid())?;
        if !response.status().is_success() {
            return Err(invalid());
        }
        runner::bytes(response, MAX_FILE).await
    }
}
