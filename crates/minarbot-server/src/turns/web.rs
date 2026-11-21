use super::{error, exists, string, AppState, Failure, Result};
use axum::http::Method;
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
    search: bool,
}

pub(crate) fn access(state: &AppState, db: &Connection, agent: &str) -> Result<Access> {
    let fetch = exists(
        db,
        "SELECT 1 FROM agents WHERE id=? AND web_access=1",
        [agent],
    )?;
    Ok(Access {
        fetch,
        search: fetch && SearchBackend::configured(state, db)?.is_some(),
    })
}

impl Access {
    pub(crate) fn offered(&self, name: &str) -> bool {
        match name {
            "web_fetch" => self.fetch,
            "web_search" => self.search,
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
            || !access(state, db, string(turn, "agent"))?.offered(name)
        {
            return Err(error(403, "Web tool is unavailable for this turn."));
        }
    }
    Ok(match call(state, name, args).await {
        Ok(value) => value,
        Err(failure) => json!({"error": failure.message, "status": failure.status}),
    })
}

pub(crate) async fn call(state: &AppState, name: &str, args: &Value) -> Result<Value> {
    let operation = async {
        match name {
            "web_fetch" if super::closed(args, &["url"]) => {
                web_fetch(string(args, "url"), FetchPolicy::default()).await
            }
            "web_search" if super::closed(args, &["query", "max_results"]) => {
                let query = string(args, "query").trim();
                if query.is_empty() || query.len() > 1000 || query.chars().any(char::is_control) {
                    return Err(error(400, "Enter a search query of up to 1000 bytes."));
                }
                let limit = match args.get("max_results") {
                    None => 5,
                    Some(value) => {
                        value
                            .as_u64()
                            .filter(|n| (1..=10).contains(n))
                            .ok_or_else(|| {
                                error(400, "max_results must be an integer from 1 to 10.")
                            })?
                    }
                };
                let backend = {
                    let guard = state.database.lock().map_err(|_| Failure::storage())?;
                    SearchBackend::configured(state, &guard.connection)?
                        .ok_or_else(|| error(503, "Web search backend is unavailable."))?
                };
                Ok(json!({"results": search_results(backend, query, limit as usize).await?}))
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

#[derive(Default)]
struct FetchPolicy {
    #[cfg(test)]
    loopback: Option<SocketAddr>,
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

async fn body(mut response: Response) -> Result<Vec<u8>> {
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

async fn web_fetch(raw: &str, policy: FetchPolicy) -> Result<Value> {
    let mut url = http_url(raw)?;
    for hop in 0..=MAX_REDIRECTS {
        let addresses = vetted_addresses(&url, &policy).await?;
        let response = client()
            .dns_resolver(Arc::new(VettedAddresses(addresses)))
            .build()
            .map_err(request_error)?
            .get(url.clone())
            .header(ACCEPT_ENCODING, "identity")
            .send()
            .await
            .map_err(request_error)?;
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
        let response = successful(response)?;
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
        let mut result = json!({"title": bounded(&title, 512), "url": url.as_str(), "text": bounded(&text, MAX_TEXT)});
        if text.len() > MAX_TEXT {
            result["note"] = json!("Text truncated to 16000 bytes.");
        }
        return Ok(result);
    }
    Err(error(502, "Web response exceeded 5 redirects."))
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

#[derive(Clone)]
pub(crate) enum SearchBackend {
    Searxng(String),
    Brave(String),
}

impl SearchBackend {
    pub(crate) fn environment() -> Option<Self> {
        Self::from_environment(
            std::env::var("MINARBOT_WEB_SEARCH_URL").ok(),
            std::env::var("MINARBOT_BRAVE_API_KEY").ok(),
        )
    }

    fn from_environment(url: Option<String>, key: Option<String>) -> Option<Self> {
        let nonempty = |value: &String| !value.trim().is_empty();
        url.filter(nonempty)
            .map(Self::Searxng)
            .or_else(|| key.filter(nonempty).map(Self::Brave))
    }

    fn configured(state: &AppState, db: &Connection) -> Result<Option<Self>> {
        if let Some(backend) = &state.config.turns.web_search {
            return Ok(Some(backend.clone()));
        }
        Ok(stored_search_url(db)?.map(Self::Searxng))
    }
}

fn stored_search_url(db: &Connection) -> Result<Option<String>> {
    Ok(db.query_row(
        "SELECT url FROM web_search_settings WHERE id=1",
        [],
        |row| row.get(0),
    )?)
}

fn absolute_http_url(raw: &str) -> Result<Url> {
    if raw.chars().any(char::is_control)
        || raw.split_once("://").is_none_or(|(scheme, authority)| {
            !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
                || authority.is_empty()
                || authority.starts_with(['/', '\\'])
        })
    {
        return Err(error(400, "Enter an absolute HTTP or HTTPS URL."));
    }
    http_url(raw)
}

fn settings_url(body: &Value) -> Result<&str> {
    body["url"]
        .as_str()
        .filter(|_| super::closed(body, &["id", "url"]))
        .map(str::trim)
        .ok_or_else(|| error(400, "Invalid web search settings."))
}

pub(crate) async fn settings(
    state: &AppState,
    method: Method,
    test: bool,
    body: &Value,
) -> Result<Value> {
    if test {
        if method != Method::POST {
            return Err(error(405, "Use POST to test web search."));
        }
        let url = absolute_http_url(settings_url(body)?)?;
        let results =
            search_results(SearchBackend::Searxng(url.into()), "test", usize::MAX).await?;
        return Ok(json!({"count": results.len()}));
    }
    if !matches!(method, Method::GET | Method::POST) {
        return Err(error(405, "Use GET or POST for web search settings."));
    }
    if let Some(backend) = &state.config.turns.web_search {
        if method == Method::POST {
            return Err(error(409, "Web search is set by the server."));
        }
        let url = match backend {
            SearchBackend::Searxng(base) => http_url(base.trim()).ok().map(|mut url| {
                url.set_query(None);
                url.to_string()
            }),
            SearchBackend::Brave(_) => None,
        };
        return Ok(json!({"url": url, "managed": true}));
    }
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    let db = &guard.connection;
    if method == Method::POST {
        let raw = settings_url(body)?;
        let url = if raw.is_empty() {
            None
        } else {
            Some(absolute_http_url(raw)?.to_string())
        };
        db.execute("UPDATE web_search_settings SET url=? WHERE id=1", [&url])?;
    }
    Ok(json!({"url": stored_search_url(db)?, "managed": false}))
}

async fn search_results(backend: SearchBackend, query: &str, limit: usize) -> Result<Vec<Value>> {
    let client = client().build().map_err(request_error)?;
    let (request, entries, snippet) = match backend {
        SearchBackend::Searxng(base) => {
            // This endpoint is selected by the server operator. The model supplies only the query.
            let mut url =
                http_url(base.trim()).map_err(|_| error(503, "Invalid SearXNG endpoint."))?;
            if !url.path().trim_end_matches('/').ends_with("/search") {
                url.path_segments_mut()
                    .map_err(|_| error(503, "Invalid SearXNG endpoint."))?
                    .pop_if_empty()
                    .push("search");
            }
            url.set_query(None);
            (
                client.get(url).query(&[("q", query), ("format", "json")]),
                "/results",
                "content",
            )
        }
        SearchBackend::Brave(key) => (
            client
                .get("https://api.search.brave.com/res/v1/web/search")
                .header("X-Subscription-Token", key.trim())
                .query(&[
                    ("q", query),
                    ("count", &limit.to_string()),
                    ("text_decorations", "false"),
                ]),
            "/web/results",
            "description",
        ),
    };
    let response = request
        .header(ACCEPT_ENCODING, "identity")
        .send()
        .await
        .map_err(request_error)?;
    let bytes = body(successful(response)?).await?;
    let result: Value =
        serde_json::from_slice(&bytes).map_err(|_| error(502, "Invalid web search response."))?;
    let results = result
        .pointer(entries)
        .and_then(Value::as_array)
        .ok_or_else(|| error(502, "Invalid web search response."))?;
    Ok(results
        .iter()
        .filter_map(|row| {
            let url = absolute_http_url(row["url"].as_str()?).ok()?;
            Some(json!({
                "title": bounded(string(row, "title"), 250),
                "url": url.as_str(),
                "content": bounded(string(row, snippet), 1000)
            }))
        })
        .take(limit)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        extract::{Request, State},
        response::IntoResponse,
        Router,
    };
    use std::{path::PathBuf, sync::Mutex};
    use tokio::{net::TcpListener, task::JoinHandle};

    struct Fixture(SocketAddr, JoinHandle<std::io::Result<()>>);

    impl Fixture {
        async fn start() -> crate::db::Result<Self> {
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let address = listener.local_addr()?;
            let router = Router::new().fallback(|request: Request| async move {
                match request.uri().path() {
                    "/redirect" => (StatusCode::FOUND, [(LOCATION, "/page")]).into_response(),
                    "/private" => (StatusCode::FOUND, [(LOCATION, "http://192.168.1.1/")]).into_response(),
                    "/loop" => (StatusCode::FOUND, [(LOCATION, "/loop")]).into_response(),
                    "/page" => ([(CONTENT_TYPE, "text/html; charset=UTF-8")], r#"
                        <!doctype html><html><head><title>Example &amp; fixture</title>
                        <style>hidden</style></head><body><h1>Example</h1>
                        <script>if (x < 2) { alert("hidden") }</script>
                        <template><p>hidden</p></template>
                        <p>Read <em>this</em> &#x2713;.</p></body></html>"#).into_response(),
                    "/text" => ([(CONTENT_TYPE, "text/plain")], " Heading\n\nBody ").into_response(),
                    "/json" => ([(CONTENT_TYPE, "application/json")], "{ \"a\": 1 }").into_response(),
                    "/truncate" => ([(CONTENT_TYPE, "text/plain")], format!("x{}", "é".repeat(MAX_TEXT))).into_response(),
                    "/binary" => ([(CONTENT_TYPE, "image/png")], b"\x89PNG".as_slice()).into_response(),
                    "/oversize" => ([(CONTENT_TYPE, "text/plain")], "x".repeat(MAX_BODY + 1)).into_response(),
                    "/oversize-stream" => {
                        let chunks = [Ok::<_, std::io::Error>(vec![b'x'; MAX_BODY]), Ok(vec![b'x'])];
                        ([(CONTENT_TYPE, "text/plain")], Body::from_stream(futures_util::stream::iter(chunks))).into_response()
                    }
                    "/metasearch/search" | "/second/search" => {
                        let params = url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
                            .collect::<std::collections::HashMap<_, _>>();
                        if params.get("format").is_none_or(|v| v != "json")
                            || params.get("q").is_none_or(|v| !matches!(v.as_ref(), "coffee & tea" | "test"))
                        {
                            return StatusCode::BAD_REQUEST.into_response();
                        }
                        if request.uri().path() == "/second/search" {
                            return axum::Json(json!({"results":[
                                {"title":"New source","url":"https://example.com/new","content":"New snippet"}
                            ]})).into_response();
                        }
                        axum::Json(json!({"results":[
                            {"title":"Invalid source","url":"javascript:alert(1)","content":"Skip"},
                            {"title":"Relative link","url":"/broken","content":"Skip"},
                            {"title":"Malformed link","url":"https:broken.example","content":"Skip"},
                            {"title":"First source","url":"https://example.com/one","content":"First snippet"},
                            {"title":"Second source","url":"https://example.com/two","content":"Second snippet"}
                        ]})).into_response()
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

    #[tokio::test]
    async fn search_reads_searxng_results_with_query_and_limit() -> crate::db::Result<()> {
        let fixture = Fixture::start().await?;
        let result = search_results(
            SearchBackend::Searxng(fixture.url("/metasearch/")),
            "coffee & tea",
            1,
        )
        .await
        .map_err(|e| e.message)?;
        assert_eq!(
            result,
            vec![
                json!({"title":"First source","url":"https://example.com/one","content":"First snippet"})
            ]
        );
        Ok(())
    }

    struct SettingsFixture {
        directory: PathBuf,
        state: AppState,
    }

    impl SettingsFixture {
        fn new() -> crate::db::Result<Self> {
            let directory =
                std::env::temp_dir().join(format!("minarbot-web-{}", uuid::Uuid::new_v4()));
            let config = serde_json::from_value::<crate::Config>(
                json!({"database": directory.join("minarbot.sqlite")}),
            )?;
            let database = crate::db::open(&config.database, false, 0)?;
            crate::data::initialize(&database)?;
            database.connection.execute(
                "INSERT INTO agents(id,name,model,instructions) VALUES('researcher','Researcher','model','')",
                [],
            )?;
            let computer = Arc::new(crate::computer::Manager::new(&config)?);
            Ok(Self {
                directory,
                state: AppState {
                    database: Arc::new(Mutex::new(database)),
                    config: Arc::new(config),
                    origin: "http://127.0.0.1:4434".into(),
                    auth: None,
                    events: crate::events::Hub::new(),
                    turns: crate::turns::Runtime::default(),
                    computer,
                    whatsapp: Default::default(),
                    trial_pending: Arc::new(false.into()),
                },
            })
        }

        async fn request(
            &self,
            path: &str,
            url: Option<&str>,
            origin: &str,
        ) -> crate::db::Result<(u16, Value)> {
            let mut request = Request::builder()
                .uri(path)
                .header("host", "127.0.0.1:4434")
                .header("origin", origin);
            let body = if let Some(url) = url {
                request = request
                    .method(Method::POST)
                    .header("content-type", "application/json");
                Body::from(json!({"id": uuid::Uuid::new_v4().to_string(), "url": url}).to_string())
            } else {
                Body::empty()
            };
            let response = crate::dispatch(State(self.state.clone()), request.body(body)?).await;
            let status = response.status().as_u16();
            let bytes = axum::body::to_bytes(response.into_body(), 64000).await?;
            Ok((status, serde_json::from_slice(&bytes)?))
        }

        fn offers_search(&self) -> crate::db::Result<bool> {
            let guard = self
                .state
                .database
                .lock()
                .map_err(|_| "Database lock poisoned")?;
            Ok(access(&self.state, &guard.connection, "researcher")
                .map_err(|e| e.message)?
                .offered("web_search"))
        }
    }

    impl Drop for SettingsFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    const SETTINGS: &str = "/api/settings/web-search";
    const TEST: &str = "/api/settings/web-search/test";

    #[tokio::test]
    async fn search_settings_test_and_apply_live() -> crate::db::Result<()> {
        let fixture = Fixture::start().await?;
        let app = SettingsFixture::new()?;
        let origin = &app.state.origin;
        let base = fixture.url("/metasearch/");
        assert_eq!(
            app.request(SETTINGS, None, origin).await?,
            (200, json!({"url":null,"managed":false}))
        );
        assert!(!app.offers_search()?);
        assert_eq!(
            app.request(TEST, Some(&base), "http://wrong.test").await?.0,
            403
        );
        assert_eq!(
            app.request(TEST, Some(&base), origin).await?,
            (200, json!({"count":2}))
        );
        assert_eq!(
            app.request(TEST, Some(&fixture.url("/missing")), origin)
                .await?,
            (502, json!({"error":"Web server returned HTTP 404."}))
        );
        assert!(!app.offers_search()?);
        assert_eq!(
            app.request(SETTINGS, Some(&base), origin).await?,
            (200, json!({"url":base,"managed":false}))
        );
        assert!(app.offers_search()?);
        let args = json!({"query":"coffee & tea","max_results":1});
        let result = call(&app.state, "web_search", &args)
            .await
            .map_err(|e| e.message)?;
        assert_eq!(result["results"][0]["url"], "https://example.com/one");
        assert_eq!(
            app.request(SETTINGS, Some(&fixture.url("/second")), origin)
                .await?
                .0,
            200
        );
        let result = call(&app.state, "web_search", &args)
            .await
            .map_err(|e| e.message)?;
        assert_eq!(result["results"][0]["url"], "https://example.com/new");
        assert_eq!(app.request(SETTINGS, Some(""), origin).await?.0, 200);
        assert!(!app.offers_search()?);
        assert_eq!(
            call(&app.state, "web_search", &args)
                .await
                .map_err(|e| e.status),
            Err(503)
        );
        Ok(())
    }

    #[tokio::test]
    async fn search_settings_validate_and_respect_environment_precedence() -> crate::db::Result<()>
    {
        let fixture = Fixture::start().await?;
        let mut app = SettingsFixture::new()?;
        let origin = app.state.origin.clone();
        for value in [
            "//localhost",
            "http:localhost",
            "https:localhost/?target=https://example.com",
            "https:///localhost",
            "file:///tmp/test",
            "https://user:pass@localhost",
        ] {
            assert_eq!(
                app.request(SETTINGS, Some(value), &origin).await?.0,
                400,
                "{value}"
            );
            assert_eq!(
                app.request(TEST, Some(value), &origin).await?.0,
                400,
                "{value}"
            );
        }
        assert_eq!(
            app.request(SETTINGS, Some("http://192.168.0.1"), &origin)
                .await?
                .0,
            200
        );
        let base = fixture.url("/second");
        let key = Some("test-brave-key".to_owned());
        Arc::make_mut(&mut app.state.config).turns.web_search =
            SearchBackend::from_environment(Some(base.clone()), key.clone());
        assert_eq!(
            app.request(SETTINGS, None, &origin).await?,
            (200, json!({"url":base,"managed":true}))
        );
        assert_eq!(app.request(SETTINGS, Some(""), &origin).await?.0, 409);
        let result = call(&app.state, "web_search", &json!({"query":"coffee & tea"}))
            .await
            .map_err(|e| e.message)?;
        assert_eq!(result["results"][0]["url"], "https://example.com/new");
        Arc::make_mut(&mut app.state.config).turns.web_search =
            SearchBackend::from_environment(Some(" ".into()), key);
        assert_eq!(
            app.request(SETTINGS, None, &origin).await?,
            (200, json!({"url":null,"managed":true}))
        );
        {
            let guard = app
                .state
                .database
                .lock()
                .map_err(|_| "Database lock poisoned")?;
            assert!(
                matches!(SearchBackend::configured(&app.state, &guard.connection).map_err(|e| e.message)?, Some(SearchBackend::Brave(key)) if key == "test-brave-key")
            );
        }
        Arc::make_mut(&mut app.state.config).turns.web_search = None;
        assert_eq!(
            app.request(SETTINGS, None, &origin).await?,
            (200, json!({"url":"http://192.168.0.1/","managed":false}))
        );
        Ok(())
    }
}
