use reqwest::{redirect, Client, Response, StatusCode};
use serde_json::Value;
use std::time::Duration;
use url::{Host, Url};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const MAX_BODY: usize = 512 * 1024;

pub fn origin(input: &str) -> Result<Url> {
    if input.len() > 2048 {
        return Err("Enter a valid server URL.".into());
    }
    let url = Url::parse(input.trim()).map_err(|_| "Enter a valid server URL.")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(
            "Use a server origin without credentials, path, query parameters, or fragment.".into(),
        );
    }
    if url.scheme() != "https" && (url.scheme() != "http" || !loopback(&url)) {
        return Err("Use HTTPS, or HTTP on localhost.".into());
    }
    Ok(url)
}

pub fn loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(domain)) => domain == "localhost",
        Some(Host::Ipv4(ip)) => ip == std::net::Ipv4Addr::LOCALHOST,
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

pub fn same_origin(url: &Url, configured: &Url) -> bool {
    matches!(url.scheme(), "http" | "https") && url.origin() == configured.origin()
}
pub fn external(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https") && url.host().is_some()
}

async fn body(mut response: Response) -> Result<String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > MAX_BODY {
            return Err("Server identity response exceeds 512 KiB.".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8(bytes)?)
}

pub(crate) fn client(timeout: Duration) -> Result<Client> {
    Ok(Client::builder()
        .timeout(timeout)
        .redirect(redirect::Policy::none())
        .no_proxy()
        .build()?)
}

async fn get(client: &Client, url: Url) -> Result<Response> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| "Could not reach the server. Check the address and connection.")?;
    if response.status().is_redirection() {
        return Err("Server redirected. Enter its final HTTPS origin.".into());
    }
    Ok(response)
}

pub(crate) async fn identity(response: Response) -> Result<Value> {
    let value: Value = serde_json::from_str(&body(response).await?)
        .map_err(|_| "Environment endpoint did not identify a Minarbot server.")?;
    if !["id", "environmentId", "environment"]
        .iter()
        .any(|key| value.get(key).is_some())
    {
        return Err("Environment endpoint did not identify a Minarbot server.".into());
    }
    Ok(value)
}

pub async fn probe(url: &Url) -> Result<()> {
    let client = client(Duration::from_secs(8))?;
    let response = get(&client, url.join("/api/environment")?).await?;
    if response.status().is_success() {
        identity(response).await?;
    } else if response.status() == StatusCode::NOT_FOUND {
        let response = get(&client, url.clone()).await?;
        let html_response = response.status().is_success()
            && response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.to_ascii_lowercase().contains("text/html"));
        let html = body(response).await?;
        if !html_response
            || html
                .to_ascii_lowercase()
                .split_once("<title>")
                .and_then(|(_, tail)| tail.split_once("</title>"))
                .is_none_or(|(title, _)| title.trim() != "minarbot")
        {
            return Err("URL did not return the Minarbot app.".into());
        }
    } else {
        return Err(format!(
            "Environment endpoint returned HTTP {}. Check server access.",
            response.status()
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[test]
    fn origin_and_navigation_policy() -> Result<()> {
        let local = origin(" http://127.0.0.1:4317 ")?;
        assert_eq!(local.as_str(), "http://127.0.0.1:4317/");
        for valid in [
            "https://nuc.example:4317",
            "http://localhost:4317",
            "http://[::1]:4317",
        ] {
            assert!(origin(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "",
            "not a URL",
            "javascript:alert(1)",
            "http://127.0.0.2",
            "http://localhost.example",
            "http://example.com",
            "https://a@example.com",
            "https://example.com/path",
            "https://example.com/?",
            "https://example.com/#",
            "file:///tmp/index.html",
        ] {
            assert!(origin(invalid).is_err(), "{invalid}");
        }
        assert!(same_origin(&local.join("/agents")?, &local));
        assert!(!same_origin(
            &Url::parse("https://127.0.0.1:4317/")?,
            &local
        ));
        assert!(!same_origin(&Url::parse("http://127.0.0.1:1234/")?, &local));
        assert!(!same_origin(
            &Url::parse("blob:http://127.0.0.1:4317/id")?,
            &local
        ));
        Ok(())
    }
    #[test]
    fn external_navigation_only_opens_http_and_https() -> Result<()> {
        for address in ["https://other.example/", "http://other.example/"] {
            assert!(external(&Url::parse(address)?));
        }
        for address in [
            "javascript:alert(1)",
            "data:text/html,x",
            "file:///tmp/index.html",
            "blob:https://other.example/id",
        ] {
            let url = Url::parse(address)?;
            assert!(!external(&url), "{address}");
            assert!(
                !same_origin(&url, &origin("https://other.example")?),
                "{address}"
            );
        }
        Ok(())
    }

    async fn probe_fixture(replies: &[(&str, &str, &str, &str)]) -> Result<Result<()>> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = origin(&format!("http://{}", listener.local_addr()?))?;
        let serve = async move {
            for (path, status, content_type, body) in replies {
                let (socket, _) = listener.accept().await?;
                let mut socket = BufReader::new(socket);
                let mut line = String::new();
                socket.read_line(&mut line).await?;
                assert_eq!(line.trim_end(), format!("GET {path} HTTP/1.1"));
                loop {
                    line.clear();
                    if socket.read_line(&mut line).await? == 0 {
                        return Err("Client closed before sending headers".into());
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                let headers = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nLocation: /different-origin\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                socket
                    .write_all(headers.as_bytes())
                    .await
                    .map_err(|_| "Write headers")?;
                socket
                    .write_all(body.as_bytes())
                    .await
                    .map_err(|_| "Write response")?;
            }
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        };
        let (server, outcome) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(serve, probe(&url))
        })
        .await?;
        server?;
        Ok(outcome)
    }

    #[tokio::test]
    async fn missing_environment_falls_back_to_minarbot_html() -> Result<()> {
        probe_fixture(&[
            ("/api/environment", "404 Not Found", "text/plain", ""),
            ("/", "200 OK", "text/html", "<title>Minarbot</title>"),
        ])
        .await??;
        Ok(())
    }

    #[tokio::test]
    async fn environment_validation_rejects_redirects_errors_and_other_apps() -> Result<()> {
        let endpoint = "/api/environment";
        probe_fixture(&[(
            endpoint,
            "200 OK",
            "application/json",
            r#"{"environmentId":"fixture"}"#,
        )])
        .await??;
        for (status, message) in [
            ("302 Found", "redirected"),
            ("403 Forbidden", "HTTP 403"),
            ("200 OK", "did not identify"),
        ] {
            let outcome = probe_fixture(&[(endpoint, status, "application/json", "{}")]).await?;
            let error = outcome.err().ok_or("Invalid server accepted")?;
            assert!(error.to_string().contains(message), "{error}");
        }
        let outcome = probe_fixture(&[
            (endpoint, "404 Not Found", "text/plain", ""),
            ("/", "200 OK", "text/html", "<title>Other</title>"),
        ])
        .await?;
        assert!(outcome
            .err()
            .ok_or("Other app accepted")?
            .to_string()
            .contains("Minarbot app"));
        Ok(())
    }

    #[tokio::test]
    async fn rejects_oversized_identity_responses() -> Result<()> {
        let body = "x".repeat(512 * 1024 + 1);
        let outcome =
            probe_fixture(&[("/api/environment", "200 OK", "application/json", &body)]).await?;
        assert!(outcome
            .err()
            .ok_or("Oversized response accepted")?
            .to_string()
            .contains("exceeds 512 KiB"));
        Ok(())
    }
}
