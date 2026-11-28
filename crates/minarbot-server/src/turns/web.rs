use super::{error, exists, string, AppState, Failure, Result};
use html5ever::tokenizer::{
    states::RawKind, BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer,
};
use reqwest::{
    dns::{Name, Resolve, Resolving},
    header::{ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_TYPE, LOCATION},
    Client, ClientBuilder, Response, StatusCode,
};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{cell::RefCell, net::SocketAddr, sync::Arc, time::Duration};
use url::{Host, Url};

const MAX_URL: usize = 4096;
const MAX_BODY: usize = 2_000_000;
const MAX_TEXT: usize = 16_000;
const MAX_REDIRECTS: usize = 5;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Default)]
pub(crate) struct Access {
    fetch: bool,
}

pub(crate) fn access(db: &Connection, agent: &str) -> Result<Access> {
    Ok(Access {
        fetch: exists(
            db,
            "SELECT 1 FROM agents WHERE id=? AND web_access=1",
            [agent],
        )?,
    })
}

impl Access {
    pub(crate) fn offered(&self, name: &str) -> bool {
        match name {
            "web_fetch" => self.fetch,
            _ => true,
        }
    }
}

pub(super) async fn run(state: &AppState, turn: &Value, name: &str, args: &Value) -> Result<Value> {
    {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        if super::profile::setup(db, turn)?
            || !exists(
                db,
                "SELECT 1 FROM turns WHERE id=? AND agent=? AND status='running'",
                [string(turn, "id"), string(turn, "agent")],
            )?
            || !access(db, string(turn, "agent"))?.offered(name)
        {
            return Err(error(403, "Web tool is unavailable for this turn."));
        }
    }
    Ok(match call(name, args).await {
        Ok(value) => value,
        Err(failure) => json!({"error": failure.message, "status": failure.status}),
    })
}

pub(crate) async fn call(name: &str, args: &Value) -> Result<Value> {
    let operation = async {
        match name {
            "web_fetch" if super::closed(args, &["url"]) => {
                web_fetch(string(args, "url"), FetchPolicy::default()).await
            }
            _ => Err(error(400, "Invalid web tool arguments.")),
        }
    };
    tokio::time::timeout(TOTAL_TIMEOUT, operation)
        .await
        .map_err(|_| error(504, "Web request timed out."))?
}

fn http_url(raw: &str) -> Result<Url> {
    if raw.len() > MAX_URL {
        return Err(error(400, "Web URL is too long."));
    }
    let mut url = Url::parse(raw).map_err(|_| error(400, "Invalid web URL."))?;
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        return Err(error(400, "Web URL must use HTTP or HTTPS."));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(error(400, "Web URLs with credentials are not allowed."));
    }
    url.set_fragment(None);
    if url.as_str().len() > MAX_URL {
        return Err(error(400, "Web URL is too long."));
    }
    Ok(url)
}

fn public_address(address: SocketAddr) -> bool {
    let ip = address.ip().to_canonical();
    if crate::computer::denied_outbound_ip(ip) {
        return false;
    }
    match ip {
        std::net::IpAddr::V4(ip) => !matches!(
            ip.octets(),
            [192, 0, 0 | 2, _]
                | [192, 88, 99, _]
                | [198, 18 | 19, _, _]
                | [198, 51, 100, _]
                | [203, 0, 113, _]
        ),
        std::net::IpAddr::V6(ip) => {
            let [first, second, ..] = ip.segments();
            // Require global unicast and exclude special-purpose, documentation and 6to4 prefixes.
            first & 0xe000 == 0x2000
                && !matches!(
                    (first, second),
                    (0x2001, 0..=0x1ff | 0xdb8) | (0x2002, _) | (0x3fff, 0..=0xfff)
                )
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct FetchPolicy {
    #[cfg(test)]
    pub(super) loopback: Option<SocketAddr>,
}

impl FetchPolicy {
    fn allows(&self, address: SocketAddr) -> bool {
        #[cfg(test)]
        if self.loopback == Some(address) && address.ip().is_loopback() {
            return true;
        }
        public_address(address)
    }
}

// A per-hop client can resolve only the addresses checked before the request.
struct VettedAddresses(Vec<SocketAddr>);

impl Resolve for VettedAddresses {
    fn resolve(&self, _: Name) -> Resolving {
        let addresses: reqwest::dns::Addrs = Box::new(self.0.clone().into_iter());
        Box::pin(std::future::ready(Ok(addresses)))
    }
}

async fn vetted_addresses(url: &Url, policy: &FetchPolicy) -> Result<Vec<SocketAddr>> {
    let port = url
        .port_or_known_default()
        .ok_or_else(|| error(400, "Invalid web URL port."))?;
    let addresses = match url.host() {
        Some(Host::Ipv4(ip)) => vec![SocketAddr::new(ip.into(), port)],
        Some(Host::Ipv6(ip)) => vec![SocketAddr::new(ip.into(), port)],
        Some(Host::Domain(host)) => {
            let name = host.trim_end_matches('.');
            if ["localhost", "local", "ts.net"].contains(&name)
                || [".localhost", ".local", ".ts.net"]
                    .iter()
                    .any(|suffix| name.ends_with(suffix))
            {
                return Err(error(403, "Web URL must resolve to public IP addresses."));
            }
            tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::lookup_host((host, port)))
                .await
                .map_err(|_| error(504, "Web DNS lookup timed out."))?
                .map_err(|_| error(502, "Web DNS lookup failed."))?
                .collect()
        }
        None => return Err(error(400, "Invalid web URL.")),
    };
    if addresses.is_empty() || addresses.iter().any(|&address| !policy.allows(address)) {
        return Err(error(403, "Web URL must resolve to public IP addresses."));
    }
    Ok(addresses)
}

fn client() -> ClientBuilder {
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(TOTAL_TIMEOUT)
        .user_agent("Minarbot/0.1")
}

fn request_error(failure: reqwest::Error) -> Failure {
    if failure.is_timeout() {
        error(504, "Web request timed out.")
    } else {
        error(502, "Web request failed.")
    }
}

fn successful(response: Response) -> Result<Response> {
    if !response.status().is_success() {
        return Err(error(
            502,
            format!("Web server returned HTTP {}.", response.status().as_u16()),
        ));
    }
    Ok(response)
}

pub(super) async fn body(mut response: Response) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY as u64)
    {
        return Err(error(413, "Web response exceeds 2 MB."));
    }
    if response
        .headers()
        .get_all(CONTENT_ENCODING)
        .iter()
        .any(|value| value != "identity")
    {
        return Err(error(
            415,
            "Web response uses an unsupported content encoding.",
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(request_error)? {
        if bytes.len() + chunk.len() > MAX_BODY {
            return Err(error(413, "Web response exceeds 2 MB."));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

// Only the rich reply module's fixed CDN URLs reach this path.
pub(crate) async fn pinned_script(raw: &str) -> Result<Vec<u8>> {
    let url = http_url(raw)?;
    let addresses = vetted_addresses(&url, &FetchPolicy::default()).await?;
    let response = successful(
        client()
            .dns_resolver(Arc::new(VettedAddresses(addresses)))
            .build()
            .map_err(request_error)?
            .get(url)
            .header(ACCEPT_ENCODING, "identity")
            .send()
            .await
            .map_err(request_error)?,
    )?;
    const LIMIT: usize = 4 * 1024 * 1024;
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(request_error)? {
        if bytes.len() + chunk.len() > LIMIT {
            return Err(error(413, "Pinned HTML preview script exceeds 4 MiB."));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(super) async fn get(url: &Url, policy: &FetchPolicy, bearer: Option<&str>) -> Result<Response> {
    let addresses = vetted_addresses(url, policy).await?;
    let client = client()
        .dns_resolver(Arc::new(VettedAddresses(addresses)))
        .build()
        .map_err(request_error)?;
    let request = client.get(url.clone()).header(ACCEPT_ENCODING, "identity");
    let request = match bearer {
        Some(key) => request.bearer_auth(key),
        None => request,
    };
    request.send().await.map_err(request_error)
}

async fn fetch(raw: &str, policy: &FetchPolicy) -> Result<(Url, Response)> {
    let mut url = http_url(raw)?;
    for hop in 0..=MAX_REDIRECTS {
        let response = get(&url, policy, None).await?;
        if matches!(
            response.status(),
            StatusCode::MOVED_PERMANENTLY
                | StatusCode::FOUND
                | StatusCode::SEE_OTHER
                | StatusCode::TEMPORARY_REDIRECT
                | StatusCode::PERMANENT_REDIRECT
        ) {
            if hop == MAX_REDIRECTS {
                return Err(error(502, "Web response exceeded 5 redirects."));
            }
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| error(502, "Web redirect has no valid location."))?;
            let next = url
                .join(location)
                .map_err(|_| error(502, "Invalid web redirect URL."))?;
            url = http_url(next.as_str())?;
            continue;
        }
        return Ok((url, successful(response)?));
    }
    Err(error(502, "Web response exceeded 5 redirects."))
}

pub(super) async fn fetch_json(url: &Url, policy: &FetchPolicy) -> Result<Value> {
    let (_, response) = fetch(url.as_str(), policy).await?;
    serde_json::from_slice(&body(response).await?)
        .map_err(|_| error(502, "Invalid web JSON response."))
}

async fn web_fetch(raw: &str, policy: FetchPolicy) -> Result<Value> {
    fetch_text(raw, policy, MAX_TEXT).await
}

pub(super) async fn fetch_text(raw: &str, policy: FetchPolicy, limit: usize) -> Result<Value> {
    let (url, response) = fetch(raw, &policy).await?;
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let html = matches!(content_type.as_str(), "text/html" | "application/xhtml+xml");
    let textual = html
        || content_type.starts_with("text/")
        || content_type == "application/json"
        || content_type.starts_with("application/") && content_type.ends_with("+json");
    if !textual {
        return Err(error(415, "Web response is not HTML, text or JSON."));
    }
    let bytes = body(response).await?;
    let text = std::str::from_utf8(&bytes)
        .ok()
        .filter(|text| {
            !text
                .bytes()
                .any(|b| b.is_ascii_control() && !b.is_ascii_whitespace())
        })
        .ok_or_else(|| error(415, "Web response must be UTF-8 text."))?;
    let (title, text) = if html {
        html_text(text)
    } else {
        (String::new(), text.to_owned())
    };
    let mut result =
        json!({"title": bounded(&title, 512), "url": url.as_str(), "text": bounded(&text, limit)});
    if text.len() > limit {
        result["note"] = json!(format!("Text truncated to {limit} bytes."));
    }
    Ok(result)
}

fn bounded(text: &str, bytes: usize) -> &str {
    &text[..text.floor_char_boundary(bytes.min(text.len()))]
}

#[derive(Default)]
struct HtmlText {
    title: String,
    text: String,
    raw: Option<String>,
    templates: usize,
}

struct HtmlSink(RefCell<HtmlText>);

impl TokenSink for HtmlSink {
    type Handle = ();

    fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
        let mut page = self.0.borrow_mut();
        match token {
            Token::CharacterTokens(chars) if page.templates == 0 => match page.raw.as_deref() {
                Some("title") => page.title.push_str(&chars),
                None => page.text.push_str(&chars),
                _ => {}
            },
            Token::TagToken(tag) => {
                let name = tag.name.as_ref();
                let opening = tag.kind == TagKind::StartTag;
                if page.raw.as_deref() == Some(name) && !opening {
                    page.raw = None;
                } else if opening {
                    let raw = match name {
                        "title" | "textarea" => Some(RawKind::Rcdata),
                        "style" | "iframe" | "xmp" | "noembed" | "noframes" => {
                            Some(RawKind::Rawtext)
                        }
                        "script" => Some(RawKind::ScriptData),
                        _ => None,
                    };
                    if let Some(kind) = raw {
                        page.raw = Some(name.to_owned());
                        return TokenSinkResult::RawData(kind);
                    }
                }
                if name == "template" {
                    page.templates = if opening {
                        page.templates + 1
                    } else {
                        page.templates.saturating_sub(1)
                    };
                }
                if page.templates == 0
                    && matches!(
                        name,
                        "address"
                            | "article"
                            | "aside"
                            | "blockquote"
                            | "br"
                            | "div"
                            | "footer"
                            | "h1"
                            | "h2"
                            | "h3"
                            | "h4"
                            | "h5"
                            | "h6"
                            | "header"
                            | "hr"
                            | "li"
                            | "main"
                            | "nav"
                            | "p"
                            | "section"
                            | "td"
                            | "th"
                            | "tr"
                    )
                {
                    page.text.push('\n');
                }
            }
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

fn html_text(html: &str) -> (String, String) {
    // ponytail: Token-level text extraction ignores CSS and layout. Use a DOM-based reader if needed.
    let tokenizer = Tokenizer::new(HtmlSink(RefCell::default()), Default::default());
    let input = BufferQueue::default();
    input.push_back(html.into());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    let page = tokenizer.sink.0.into_inner();
    let title = page.title.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = page
        .text
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (title, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, extract::Request, response::IntoResponse, Router};
    use tokio::{net::TcpListener, task::JoinHandle};

    struct Fixture(SocketAddr, JoinHandle<std::io::Result<()>>);

    impl Fixture {
        async fn start() -> crate::db::Result<Self> {
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let address = listener.local_addr()?;
            let router = Router::new().fallback(|request: Request| async move {
                match request.uri().path() {
                    "/redirect" => (StatusCode::FOUND, [(LOCATION, "/page")]).into_response(),
                    "/private" => {
                        (StatusCode::FOUND, [(LOCATION, "http://192.168.1.1/")]).into_response()
                    }
                    "/loop" => (StatusCode::FOUND, [(LOCATION, "/loop")]).into_response(),
                    "/page" => (
                        [(CONTENT_TYPE, "text/html; charset=UTF-8")],
                        r#"
                        <!doctype html><html><head><title>Example &amp; fixture</title>
                        <style>hidden</style></head><body><h1>Example</h1>
                        <script>if (x < 2) { alert("hidden") }</script>
                        <template><p>hidden</p></template>
                        <p>Read <em>this</em> &#x2713;.</p></body></html>"#,
                    )
                        .into_response(),
                    "/text" => {
                        ([(CONTENT_TYPE, "text/plain")], " Heading\n\nBody ").into_response()
                    }
                    "/json" => {
                        ([(CONTENT_TYPE, "application/json")], "{ \"a\": 1 }").into_response()
                    }
                    "/truncate" => (
                        [(CONTENT_TYPE, "text/plain")],
                        format!("x{}", "é".repeat(MAX_TEXT)),
                    )
                        .into_response(),
                    "/binary" => {
                        ([(CONTENT_TYPE, "image/png")], b"\x89PNG".as_slice()).into_response()
                    }
                    "/oversize" => {
                        ([(CONTENT_TYPE, "text/plain")], "x".repeat(MAX_BODY + 1)).into_response()
                    }
                    "/oversize-stream" => {
                        let chunks = [
                            Ok::<_, std::io::Error>(vec![b'x'; MAX_BODY]),
                            Ok(vec![b'x']),
                        ];
                        (
                            [(CONTENT_TYPE, "text/plain")],
                            Body::from_stream(futures_util::stream::iter(chunks)),
                        )
                            .into_response()
                    }
                    _ => StatusCode::NOT_FOUND.into_response(),
                }
            });
            Ok(Self(
                address,
                tokio::spawn(async move { axum::serve(listener, router).await }),
            ))
        }

        fn url(&self, path: &str) -> String {
            format!("http://{}{path}", self.0)
        }

        async fn fetch(&self, path: &str) -> Result<Value> {
            web_fetch(
                &self.url(path),
                FetchPolicy {
                    loopback: Some(self.0),
                },
            )
            .await
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.1.abort();
        }
    }

    #[tokio::test]
    async fn fetch_reads_html_text_and_json() -> crate::db::Result<()> {
        let fixture = Fixture::start().await?;
        let html = fixture.fetch("/redirect").await.map_err(|e| e.message)?;
        assert_eq!(
            html,
            json!({"title":"Example & fixture","url":fixture.url("/page"),"text":"Example\nRead this ✓."})
        );
        for (path, expected) in [("/text", " Heading\n\nBody "), ("/json", "{ \"a\": 1 }")] {
            let response = fixture.fetch(path).await.map_err(|e| e.message)?;
            assert_eq!(response["text"], expected);
        }
        let clipped = fixture.fetch("/truncate").await.map_err(|e| e.message)?;
        let text = clipped["text"].as_str().ok_or("Missing text")?;
        assert_eq!(text.len(), MAX_TEXT - 1);
        assert!(clipped["note"]
            .as_str()
            .is_some_and(|note| note.contains("truncated")));
        Ok(())
    }

    #[tokio::test]
    async fn fetch_refuses_non_public_addresses_and_credentials() {
        for host in [
            "0.0.0.0",
            "127.0.0.1",
            "127.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "224.0.0.1",
            "240.0.0.1",
            "198.18.0.1",
            "[::]",
            "[::1]",
            "[fc00::1]",
            "[fd7a:115c:a1e0::1]",
            "[fe80::1]",
            "[ff02::1]",
            "[::ffff:127.0.0.1]",
            "[::ffff:100.64.0.1]",
            "[::ffff:192.168.1.1]",
            "[64:ff9b::a00:1]",
            "[2002:7f00:1::1]",
            "localhost",
            "node.ts.net.",
            "router.local",
        ] {
            let result = web_fetch(&format!("http://{host}/"), FetchPolicy::default()).await;
            assert_eq!(result.map_err(|e| e.status), Err(403), "{host}");
        }
        for url in [
            "https://user:pass@example.com/",
            "https://user@example.com/",
            "https://:pass@example.com/",
            "file:///tmp/local",
        ] {
            let result = web_fetch(url, FetchPolicy::default()).await;
            assert_eq!(result.map_err(|e| e.status), Err(400), "{url}");
        }
    }

    #[tokio::test]
    async fn fetch_refuses_unsafe_redirects_and_bodies() -> crate::db::Result<()> {
        let fixture = Fixture::start().await?;
        let result = web_fetch(&fixture.url("/page"), FetchPolicy::default()).await;
        assert_eq!(result.map_err(|e| e.status), Err(403));
        for (path, expected) in [
            ("/private", 403),
            ("/loop", 502),
            ("/binary", 415),
            ("/oversize", 413),
            ("/oversize-stream", 413),
        ] {
            assert_eq!(
                fixture.fetch(path).await.map_err(|e| e.status),
                Err(expected),
                "{path}"
            );
        }
        Ok(())
    }
}
