use super::*;
use axum::{http::HeaderValue, response::IntoResponse};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};
use tokio::sync::OnceCell;

pub const POLICY:&str="default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src 'none'; font-src 'none'; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'";
static SIZE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^(?:0|[0-9]{1,3}(?:\.[0-9]{1,2})?(?:px|em|rem|%))(?:\s+(?:0|[0-9]{1,3}(?:\.[0-9]{1,2})?(?:px|em|rem|%))){0,3}$").unwrap_or_else(|_|unreachable!())
});
static COLOR: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)^(?:#[a-f0-9]{3,8}|[a-z]{1,20}|rgba?\([0-9.,%\s]+\))$")
        .unwrap_or_else(|_| unreachable!())
});
fn style(value: &str) -> Option<String> {
    let styles: Vec<_> = value
        .split(';')
        .filter_map(|s| {
            let (key, value) = s.split_once(':')?;
            let key = key.trim();
            let value = value.trim();
            let valid = match key {
                "color" | "background-color" | "border-color" => COLOR.is_match(value),
                "padding" | "margin" | "gap" | "width" | "max-width" | "min-width"
                | "border-width" | "border-radius" | "font-size" => SIZE.is_match(value),
                "font-weight" => [
                    "normal", "bold", "100", "200", "300", "400", "500", "600", "700", "800", "900",
                ]
                .contains(&value),
                "font-style" => ["normal", "italic"].contains(&value),
                "text-align" => {
                    ["left", "right", "center", "justify", "start", "end"].contains(&value)
                }
                "display" => [
                    "block",
                    "inline",
                    "inline-block",
                    "flex",
                    "grid",
                    "table",
                    "none",
                ]
                .contains(&value),
                "flex-direction" => ["row", "column"].contains(&value),
                "flex-wrap" => ["wrap", "nowrap"].contains(&value),
                "justify-content" => {
                    ["start", "end", "center", "space-between", "space-around"].contains(&value)
                }
                "align-items" => ["start", "end", "center", "stretch"].contains(&value),
                "border-style" => ["solid", "dashed", "dotted", "none"].contains(&value),
                _ => false,
            };
            valid.then(|| format!("{key}:{value}"))
        })
        .collect();
    (!styles.is_empty()).then(|| styles.join(";"))
}
pub fn html(name: &str, bytes: &[u8]) -> Result<String> {
    let name = name.to_ascii_lowercase();
    if !name.ends_with(".html") && !name.ends_with(".htm") {
        return Err(error(
            415,
            "Visual preview only supports .html or .htm files.",
        ));
    }
    if bytes.len() > 256 * 1024 {
        return Err(error(413, "HTML preview limit is 256 KiB."));
    }
    let input = std::str::from_utf8(bytes)
        .map_err(|_| error(415, "HTML file must use UTF-8."))?
        .trim_start_matches('\u{feff}');
    let tags:HashSet<_>="article section header footer main nav aside div span p h1 h2 h3 h4 h5 h6 ul ol li dl dt dd strong em b i u s small sub sup blockquote pre code br hr table caption thead tbody tfoot tr th td details summary figure figcaption mark time".split(' ').collect();
    let attrs: HashMap<_, _> = [
        ("details", vec!["open"]),
        ("th", vec!["colspan", "rowspan", "scope"]),
        ("td", vec!["colspan", "rowspan"]),
        ("ol", vec!["start", "reversed"]),
        ("time", vec!["datetime"]),
    ]
    .into_iter()
    .map(|(tag, attrs)| (tag, attrs.into_iter().collect()))
    .collect();
    let body = ammonia::Builder::new()
        .tags(tags)
        .tag_attributes(attrs)
        .generic_attributes(["style", "title", "lang", "dir"].into_iter().collect())
        .clean_content_tags(
            [
                "script", "style", "textarea", "option", "iframe", "object", "embed", "svg",
                "math", "template",
            ]
            .into_iter()
            .collect(),
        )
        .link_rel(None)
        .attribute_filter(|_, key, value| {
            if key == "style" {
                style(value).map(std::borrow::Cow::Owned)
            } else {
                Some(std::borrow::Cow::Borrowed(value))
            }
        })
        .clean(input)
        .to_string()
        .replace(" open=\"\"", " open")
        .replace(" reversed=\"\"", " reversed");
    Ok(format!("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" content=\"{POLICY}\"><meta name=\"referrer\" content=\"no-referrer\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>HTML preview</title><style>html{{color-scheme:light}}body{{margin:24px;color:#20242b;background:#fff;font:16px/1.5 Georgia,serif;overflow-wrap:anywhere}}*{{box-sizing:border-box;max-width:100%}}pre{{white-space:pre-wrap}}table{{border-collapse:collapse;display:block;overflow:auto}}td,th{{border:1px solid #bbc0c8;padding:8px}}summary{{cursor:pointer;padding:10px;min-height:44px}}summary:focus-visible{{outline:2px solid #354aa5}}figure{{margin:0}}h1,h2,h3{{line-height:1.2}}</style></head><body>{body}</body></html>"))
}
pub fn render(html: String, permissions: bool) -> Response {
    let mut response = (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CONTENT_DISPOSITION, "inline"),
        ],
        html,
    )
        .into_response();
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY,HeaderValue::from_static("default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src 'none'; font-src 'none'; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; sandbox; frame-ancestors 'self'"));
    if permissions {
        response.headers_mut().insert(
            "permissions-policy",
            HeaderValue::from_static(
                "camera=(), microphone=(), geolocation=(), payment=(), usb=()",
            ),
        );
    }
    response
}
type PdfResult = std::result::Result<String, (u16, String)>;
type PendingPdf = Arc<OnceCell<PdfResult>>;
static PENDING: LazyLock<Mutex<HashMap<String, PendingPdf>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
struct PdfWork<'a> {
    id: &'a str,
    cell: PendingPdf,
}
impl Drop for PdfWork<'_> {
    fn drop(&mut self) {
        if let Ok(mut pending) = PENDING.lock() {
            if Arc::strong_count(&self.cell) == 2
                && pending
                    .get(self.id)
                    .is_some_and(|cell| Arc::ptr_eq(cell, &self.cell))
            {
                pending.remove(self.id);
            }
        }
    }
}
pub fn forget(db: &Connection, id: &str) -> Result<()> {
    PENDING.lock().map_err(|_| Failure::storage())?.remove(id);
    db.execute("DELETE FROM pdf_text WHERE file=?", [id])?;
    Ok(())
}
pub async fn pdf(state: &AppState, meta: &Value, bytes: &[u8]) -> Result<Response> {
    pdf_with(state, meta, crate::harness::pdf::extract(state, bytes)).await
}
async fn pdf_with(
    state: &AppState,
    meta: &Value,
    extract: impl std::future::Future<Output = Result<String>>,
) -> Result<Response> {
    let id = text(meta, "id");
    let cached = {
        let db = state.database.lock().map_err(|_| Failure::storage())?;
        one(&db.connection, "SELECT p.text FROM pdf_text p JOIN files f ON f.id=p.file WHERE p.file=? AND f.deleted=0 AND f.digest=?", [id,text(meta,"digest")])?
    };
    let text = if let Some(cached) = cached {
        super::text(&cached, "text").to_owned()
    } else {
        let cell = {
            let mut pending = PENDING.lock().map_err(|_| Failure::storage())?;
            if let Some(prior) = pending.get(id) {
                prior.clone()
            } else {
                if pending.len() >= 2 {
                    return Err(error(
                        429,
                        "Two PDFs are being read. Wait briefly, then try again.",
                    ));
                }
                let work = Arc::new(OnceCell::new());
                pending.insert(id.to_owned(), work.clone());
                work
            }
        };
        let work = PdfWork { id, cell };
        let result = work
            .cell
            .get_or_init(|| async {
                let result = async {
                    let text = extract.await?;
                    let db = state.database.lock().map_err(|_| Failure::storage())?;
                    let pending = PENDING.lock().map_err(|_| Failure::storage())?;
                    if !pending
                        .get(id)
                        .is_some_and(|current| Arc::ptr_eq(current, &work.cell))
                        || one(
                            &db.connection,
                            "SELECT id FROM files WHERE id=? AND deleted=0 AND digest=?",
                            [id, super::text(meta, "digest")],
                        )?
                        .is_none()
                    {
                        return Err(error(409, "File changed while reading. Open it again."));
                    }
                    db.connection
                        .execute("INSERT OR IGNORE INTO pdf_text VALUES(?,?)", [id, &text])?;
                    Ok(text)
                }
                .await;
                result.map_err(|e: Failure| (e.status, e.message))
            })
            .await
            .clone();
        result.map_err(|(status, message)| error(status, message))?
    };
    Ok(json_response(
        200,
        json!({"id":id,"name":meta["name"],"text":text,"source":"pdf-text","note":"Extracted text. Layout, images, and scan results are not included."}),
    ))
}
pub async fn extract_pdf(bytes: &[u8]) -> Result<String> {
    if !crate::platform::LINUX_SANDBOX {
        return Err(error(503, "PDF sandbox is unavailable on this platform."));
    }
    extract_pdf_with(std::path::Path::new("/usr/bin/bwrap"), bytes).await
}

struct PdfChild(tokio::process::Child);
impl PdfChild {
    fn kill_group(&self) {
        if let Some(pid) = self
            .0
            .id()
            .and_then(|pid| rustix::process::Pid::from_raw(pid as i32))
        {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
    }
}
impl Drop for PdfChild {
    fn drop(&mut self) {
        self.kill_group();
    }
}

async fn extract_pdf_with(launcher: &std::path::Path, bytes: &[u8]) -> Result<String> {
    use std::{os::unix::process::CommandExt, process::Stdio};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    if !bytes.starts_with(b"%PDF-") || bytes.len() > 10 * 1024 * 1024 {
        return Err(error(415, "PDF is invalid or exceeds 10 MiB."));
    }
    let mut command = tokio::process::Command::new(launcher);
    command
        .args([
            "--unshare-all",
            "--die-with-parent",
            "--new-session",
            "--clearenv",
            "--ro-bind",
            "/usr/bin/pdftotext",
            "/usr/bin/pdftotext",
            "--ro-bind",
            "/usr/bin/prlimit",
            "/usr/bin/prlimit",
            "--ro-bind",
            "/usr/lib",
            "/usr/lib",
            "--symlink",
            "usr/lib",
            "/lib",
            "--ro-bind",
            "/lib64",
            "/lib64",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--chdir",
            "/tmp",
            "--setenv",
            "LANG",
            "C.UTF-8",
            "/usr/bin/prlimit",
            "--as=268435456",
            "--cpu=3",
            "--fsize=10485760",
            "--nofile=64",
            "--nproc=16",
            "/usr/bin/pdftotext",
            "-enc",
            "UTF-8",
            "-eol",
            "unix",
            "-nopgbrk",
            "-",
            "-",
        ])
        .env_clear()
        .env("LANG", "C.UTF-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command.as_std_mut().process_group(0);
    let mut child = PdfChild(command.spawn().map_err(|_| {
        error(
            503,
            "PDF sandbox is unavailable. Check Bubblewrap and Poppler on the host.",
        )
    })?);
    let mut stdin = child.0.stdin.take().ok_or_else(Failure::storage)?;
    let stdout = child.0.stdout.take().ok_or_else(Failure::storage)?;
    let stderr = child.0.stderr.take().ok_or_else(Failure::storage)?;
    let work = async {
        let write = async {
            let _ = stdin.write_all(bytes).await;
            drop(stdin);
        };
        let read = async {
            let mut out = Vec::new();
            stdout
                .take(256 * 1024 + 1)
                .read_to_end(&mut out)
                .await
                .map_err(|_| Failure::storage())?;
            Ok::<_, Failure>(out)
        };
        let err = async {
            let mut out = Vec::new();
            stderr
                .take(256)
                .read_to_end(&mut out)
                .await
                .map_err(|_| Failure::storage())?;
            Ok::<_, Failure>(out)
        };
        let (_, out, err) = tokio::join!(write, read, err);
        let out = out?;
        let err = err?;
        if out.len() > 256 * 1024 {
            return Err(error(
                413,
                "PDF text exceeds 256 KiB. Use a smaller document.",
            ));
        }
        let status = child.0.wait().await.map_err(|_| Failure::storage())?;
        if !status.success() {
            return Err(
                if err.starts_with(b"bwrap:") || err.starts_with(b"prlimit:") {
                    error(503,"PDF sandbox is unavailable. Check Bubblewrap, Poppler, and host resource limits, then try again.")
                } else {
                    error(
                        415,
                        "PDF cannot be read. Check the document and password protection.",
                    )
                },
            );
        }
        let text = String::from_utf8(out).map_err(|_| {
            error(
                415,
                "PDF has no readable text. Run OCR for scanned documents first.",
            )
        })?;
        if text.trim().is_empty()
            || text.chars().any(|c| {
                c <= '\x08' || c == '\x0b' || c == '\x0c' || ('\x0e'..='\x1f').contains(&c)
            })
        {
            return Err(error(
                415,
                "PDF has no readable text. Run OCR for scanned documents first.",
            ));
        }
        Ok(text)
    };
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), work)
        .await
        .unwrap_or_else(|_| Err(error(415, "PDF reading time limit exceeded.")));
    if result.is_err() {
        child.kill_group();
        let _ = child.0.kill().await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_preview_removes_active_content_and_keeps_details_and_layout() -> Result<()> {
        let payload = r##"<!doctype html><html><head><base href="https://evil.example"><meta http-equiv="refresh" content="0;url=https://evil.example"><style>@import 'https://evil.example/a';</style></head><body onload="alert(1)">
    <script>window.parent.fetch('/api/agents')</script><iframe srcdoc="<script>alert(1)</script>"></iframe>
    <object data="https://evil.example"></object><embed src="https://evil.example"><svg><a href="https://evil.example"><text>evil</text></a></svg><math><annotation-xml encoding="text/html"><script>alert(1)</script></annotation-xml></math>
    <img src="https://evil.example" srcset="https://evil.example"><video poster="https://evil.example"><source src="https://evil.example"></video>
    <form action="/api/agents"><input name="name"><button formaction="https://evil.example">Submit</button></form>
    <a href="javascript:alert(1)" ping="https://evil.example" target="_top">Readable link text</a>
    <details open><summary onclick="alert(1)">Expand me</summary><p style="color:#123456; padding:12px; background-image:url(https://evil.example); position:fixed; font-family:evil">Useful text</p></details>
    <table><tr><th scope="col">Name</th><td colspan="2">Value</td></tr></table></body></html>"##;
        let output = html("Report.HTML", payload.as_bytes())?;
        for kept in [
            POLICY,
            "<details open><summary>Expand me</summary>",
            "color:#123456;padding:12px",
            r#"<th scope="col">Name</th>"#,
            "Readable link text",
        ] {
            assert!(output.contains(kept), "missing {kept}");
        }

        let removed = regex::Regex::new(
            r"(?i)evil\.example|window\.parent|alert\(1\)|background-image|position:fixed|font-family:evil|<(?:script|iframe|object|embed|svg|math|form|input|button|img|video|source|a|base)\b|\s(?:on\w+|href|src|srcdoc|srcset|formaction|action|ping|target)=",
        ).map_err(|_| Failure::storage())?;
        assert!(!removed.is_match(&output), "{output}");
        let active =
            regex::Regex::new(r"(?i)<(?:script|img|svg|math|template)\b|expression\(|url\(")
                .map_err(|_| Failure::storage())?;
        for malformed in [
            "<scr<script>ipt>alert(1)</scr</script>ipt>",
            r#"<math><mtext><table><mglyph><style><!--</style><img title="--><img src=x onerror=alert(1)>">"#,
            r#"<noscript><p title="</noscript><img src=x onerror=alert(1)>">test"#,
            r#"<p style="color:expression(alert(1));width:calc(1px + url(x));padding:var(--x);background-color:u\72l(x)">x</p>"#,
            "<template><img src=x onerror=alert(1)></template>",
        ] {
            let safe = html("x.htm", malformed.as_bytes())?;
            assert!(!active.is_match(&safe), "{safe}");
        }

        assert!(html("x.txt", b"text").is_err());
        assert!(html("x.html", &[0xff]).is_err());
        assert!(html("x.html", &vec![0; 256 * 1024 + 1]).is_err());
        Ok(())
    }
}

#[cfg(test)]
#[path = "pdf_tests.rs"]
mod pdf_tests;
