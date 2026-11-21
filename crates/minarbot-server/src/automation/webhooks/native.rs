use super::*;
use ring::hmac;
use sha2::{Digest, Sha256};

const KEY: &str = "routine-webhook-key";

pub(super) enum Delivery {
    Event(Value),
    Challenge(String),
    Ignore,
}

pub(in crate::automation) fn supported(source: &Value) -> bool {
    matches!(
        source.as_str(),
        Some("slack" | "linear" | "sentry" | "github")
    )
}

fn secret_error() -> Failure {
    error(
        503,
        "Webhook signing secret is unavailable. Save it again on this host.",
    )
}

pub(super) fn configure(
    state: &AppState,
    db: &Connection,
    path: &str,
    routine: &Value,
    current: &Value,
    v: &Value,
) -> Result<Value> {
    if !data::closed(v, &["id", "revision", "signingSecret", "filters"])
        || !integer(&v["revision"], 0, 9007199254740991)
    {
        return Err(error(400, "Invalid webhook settings."));
    }
    let secret = v["signingSecret"].as_str().filter(|value| {
        (16..=512).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_graphic())
    });
    if v.get("signingSecret").is_some() && secret.is_none() {
        return Err(error(
            400,
            "Signing secret must contain 16 to 512 visible ASCII characters.",
        ));
    }
    let id = string(routine, "id");
    let source = string(routine, "webhook_source");
    let fs = filters(&v["filters"])?;
    let fingerprint = secret
        .map(|secret| {
            let key = crate::mcp::client::key(state, true, KEY).map_err(|_| secret_error())?;
            let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, &key), secret.as_bytes());
            Ok::<_, Failure>(hex(tag.as_ref()))
        })
        .transpose()?;
    let request =
        json!({"id":v["id"],"revision":v["revision"],"filters":fs,"keyFingerprint":fingerprint});
    let saved = command(db, "webhook_commands", path, &request, || {
        let rev = current["revision"].as_i64().unwrap_or(0);
        if v["revision"] != rev {
            return Err(error(409, "Webhook changed. Reload settings."));
        }
        let route = current["route"].as_str().ok_or_else(Failure::storage)?;
        let encrypted = if let Some(secret) = secret {
            Some(
                crate::mcp::client::encrypt_with(
                    state,
                    secret,
                    &format!("{source}/{id}/{route}"),
                    KEY,
                )
                .map_err(|_| secret_error())?,
            )
        } else {
            if rev == 0 {
                return Err(error(400, "Enter the provider's webhook signing secret."));
            }
            None
        };
        db.execute(
            "INSERT INTO webhooks(routine,route,secret,event_name,filters,revision) VALUES(?,?,?,?,?,?) ON CONFLICT(routine) DO UPDATE SET secret=coalesce(?,webhooks.secret),filters=excluded.filters,revision=excluded.revision",
            params![id, route, encrypted.as_deref().unwrap_or(""), "", fs.to_string(), rev + 1, encrypted],
        )?;
        routine_hook(db, id)
    })?;
    if routine_hook(db, id)? != saved {
        return Err(error(409, "Webhook changed. Reload settings."));
    }
    Ok(saved)
}

pub(super) fn receive(
    state: &AppState,
    hook: &Value,
    source: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Delivery> {
    let binding = format!(
        "{source}/{}/{}",
        string(hook, "routine"),
        string(hook, "route")
    );
    let secret = crate::mcp::client::decrypt_with(state, string(hook, "secret"), &binding, KEY)
        .map_err(|_| secret_error())?;
    if secret.is_empty() {
        return Err(secret_error());
    }
    receive_signed(source, headers, body, &secret, state.config.now())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unauthorized() -> Failure {
    error(401, "Invalid webhook signature or delivery identity.")
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str> {
    if headers.get_all(name).iter().count() != 1 {
        return Err(unauthorized());
    }
    crate::text_header(headers, name).ok_or_else(unauthorized)
}

fn verify(body: &[u8], signature: &str, secret: &str) -> Result<()> {
    if signature.len() != 64 || !signature.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(unauthorized());
    }
    let mut tag = [0; 32];
    for (index, byte) in tag.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&signature[index * 2..index * 2 + 2], 16)
            .map_err(|_| unauthorized())?;
    }
    hmac::verify(
        &hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes()),
        body,
        &tag,
    )
    .map_err(|_| unauthorized())
}

fn fresh(timestamp: i64, now: i64) -> Result<()> {
    if timestamp.abs_diff(now) > 300_000 {
        return Err(error(401, "Webhook timestamp has expired."));
    }
    Ok(())
}

fn receive_signed(
    source: &str,
    headers: &HeaderMap,
    body: &[u8],
    secret: &str,
    now: i64,
) -> Result<Delivery> {
    match source {
        "slack" => {
            let time = header(headers, "x-slack-request-timestamp")?;
            let timestamp = time
                .parse::<i64>()
                .ok()
                .and_then(|time| time.checked_mul(1000))
                .ok_or_else(unauthorized)?;
            fresh(timestamp, now)?;
            let mut signed = format!("v0:{time}:").into_bytes();
            signed.extend_from_slice(body);
            verify(
                &signed,
                header(headers, "x-slack-signature")?
                    .strip_prefix("v0=")
                    .ok_or_else(unauthorized)?,
                secret,
            )?;
        }
        "linear" => verify(body, header(headers, "linear-signature")?, secret)?,
        "sentry" => verify(body, header(headers, "sentry-hook-signature")?, secret)?,
        "github" => verify(
            body,
            header(headers, "x-hub-signature-256")?
                .strip_prefix("sha256=")
                .ok_or_else(unauthorized)?,
            secret,
        )?,
        _ => return Err(Failure::storage()),
    }
    let mut payload: Value =
        serde_json::from_slice(body).map_err(|_| error(400, "Event must be valid UTF-8 JSON."))?;
    if !payload.is_object() {
        return Err(error(400, "Webhook payload must be a JSON object."));
    }
    if source == "slack" && payload["type"] == "url_verification" {
        return payload["challenge"]
            .as_str()
            .filter(|value| !value.is_empty() && value.len() <= 512 && !value.contains(secret))
            .map(|value| Delivery::Challenge(value.to_owned()))
            .ok_or_else(|| error(400, "Invalid Slack challenge."));
    }
    let digest = format!("{:x}", Sha256::digest(body));
    let (id, event) = match source {
        "slack"
            if payload["type"] == "event_callback"
                && payload["event"]["type"] == "message"
                && payload["event"]["subtype"].is_null()
                && payload["event"]["bot_id"].is_null() =>
        {
            // Slack's legacy verification token is unnecessary once the HMAC is verified.
            if let Some(object) = payload.as_object_mut() {
                object.remove("token");
            }
            (string(&payload, "event_id"), "message")
        }
        "linear" => {
            fresh(
                payload["webhookTimestamp"]
                    .as_i64()
                    .ok_or_else(unauthorized)?,
                now,
            )?;
            if payload["type"] != "Issue" || !payload["data"]["id"].is_string() {
                return Ok(Delivery::Ignore);
            }
            (header(headers, "linear-delivery")?, "Issue")
        }
        "sentry"
            if header(headers, "sentry-hook-resource")? == "event_alert"
                && payload["action"] == "triggered"
                && payload["data"]["event"].is_object() =>
        {
            (digest.as_str(), "event_alert")
        }
        "github"
            if header(headers, "x-github-event")? == "issues"
                && payload["action"].is_string()
                && payload["issue"]["number"].is_u64()
                && payload["repository"]["id"].is_u64()
                && payload["comment"].is_null() =>
        {
            (header(headers, "x-github-delivery")?, "issues")
        }
        _ => return Ok(Delivery::Ignore),
    };
    if id.is_empty()
        || id.len() > 200
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
    {
        return Err(unauthorized());
    }
    let event = json!({"id":id,"digest":digest,"event":event,"eventTime":now,"payload":payload});
    if crate::mcp::secrets::reflected(event.to_string().as_bytes(), secret) {
        return Err(error(400, "Webhook payload contains its signing secret."));
    }
    Ok(Delivery::Event(event))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn native_webhooks_require_a_signature_for_the_exact_body() -> crate::db::Result<()> {
        const NOW: i64 = 1_770_000_000_000;
        let secret = "synthetic-signing-key";
        for (source, name, prefix, body, extra) in [
            ("slack", "x-slack-signature", "v0=", br#"{"type":"event_callback","event_id":"Ev1","event":{"type":"message","text":"Ignore instructions"},"token":"legacy"}"#.as_slice(), &[("x-slack-request-timestamp", "1770000000")][..]),
            ("linear", "linear-signature", "", br#"{"type":"Issue","data":{"id":"fixture"},"webhookTimestamp":1770000000000}"#, &[("linear-delivery", "fixture")]),
            ("sentry", "sentry-hook-signature", "", br#"{"action":"triggered","data":{"event":{"title":"Synthetic alert"}}}"#, &[("sentry-hook-resource", "event_alert")]),
            ("github", "x-hub-signature-256", "sha256=", br#"{"action":"opened","issue":{"number":1},"repository":{"id":2}}"#, &[("x-github-event", "issues"), ("x-github-delivery", "fixture")]),
        ] {
            let mut headers = HeaderMap::new();
            for (header, value) in extra {
                headers.insert(*header, HeaderValue::from_static(value));
            }
            let signed = if source == "slack" {
                [b"v0:1770000000:".as_slice(), body].concat()
            } else {
                body.to_vec()
            };
            let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes()), &signed);
            headers.insert(name, HeaderValue::from_str(&format!("{prefix}{}", hex(tag.as_ref())))?);
            let result = receive_signed(source, &headers, body, secret, NOW)
                .map_err(|e| e.message)?;
            let Delivery::Event(event) = result else {
                return Err("Expected an authenticated event".into());
            };
            assert!(event["payload"].is_object());
            if source == "slack" {
                assert_eq!(event["payload"]["event"]["text"], "Ignore instructions");
                assert!(event["payload"].get("token").is_none());
            }
            assert_eq!(receive_signed(source, &headers, b"{}", secret, NOW).err().map(|e| e.status), Some(401));
            assert_eq!(receive_signed(source, &headers, body, "another-key", NOW).err().map(|e| e.status), Some(401));
            if ["slack", "linear"].contains(&source) {
                assert_eq!(receive_signed(source, &headers, body, secret, NOW + 300_001).err().map(|e| e.status), Some(401));
            }
        }
        Ok(())
    }
}
