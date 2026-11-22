use super::*;
use regex::Regex;
use reqwest::{blocking::Client, header::CONTENT_TYPE, redirect::Policy, Url};
use serde_json::Value;
use std::{sync::LazyLock, time::Duration};

static COMMENTS: LazyLock<Regex> = LazyLock::new(|| release::pattern(r"(?s)<!--.*?-->"));
static TAGS: LazyLock<Regex> = LazyLock::new(|| release::pattern(r"(?i)<(script|link)\b[^>]*>"));
static ATTRS: LazyLock<Regex> = LazyLock::new(|| {
    release::pattern(r#"([^\s=<>]+)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>\x60]+))"#)
});
static SCRIPT: LazyLock<Regex> = LazyLock::new(|| {
    release::pattern(r"(?i)(?:text|application)/(?:javascript|ecmascript|x-javascript)")
});

fn get(
    client: &Client,
    origin: &Url,
    target: &Url,
    content_type: impl Fn(&str) -> bool,
    report: &mut Vec<String>,
) -> Result<String> {
    if target.origin() != origin.origin()
        || !target.username().is_empty()
        || target.password().is_some()
    {
        return Err(format!("Refusing asset outside origin: {target}").into());
    }
    let response = client
        .get(target.clone())
        .header("Origin", origin.origin().ascii_serialization())
        .header("Sec-Fetch-Site", "same-origin")
        .header("Cache-Control", "no-cache")
        .send()
        .map_err(|error| format!("GET {target} failed: {error}"))?;
    let status = response.status();
    if status.is_redirection() && response.headers().contains_key(reqwest::header::LOCATION) {
        return Err(format!("GET {target} failed: unexpected redirect").into());
    }
    if !status.is_success() {
        return Err(format!("GET {target}: HTTP {}", status.as_u16()).into());
    }
    let mime = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !content_type(mime) {
        return Err(format!("GET {target}: unexpected Content-Type {mime}").into());
    }
    let mut path = target.path().to_owned();
    if let Some(query) = target.query() {
        path.push('?');
        path.push_str(query);
    }
    report.push(format!("GET {path}: {}", status.as_u16()));
    Ok(response.text()?)
}

fn extension(url: &Url) -> &str {
    url.path()
        .rsplit_once('.')
        .map_or("", |(_, extension)| extension)
}

fn check(args: &[String]) -> Result<bool> {
    let mut origin = None;
    let mut present = Vec::new();
    let mut absent = Vec::new();
    let mut version = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let (key, inline) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(key, value)| (key, Some(value)));
        match key {
            "--present" | "--absent" | "--expect-version" => {
                let text = inline
                    .or_else(|| {
                        args.next()
                            .map(String::as_str)
                            .filter(|value| !value.starts_with("--"))
                    })
                    .filter(|value| !value.is_empty())
                    .ok_or("Markers and expected version must be non-empty text.")?;
                match key {
                    "--present" => present.push(text),
                    "--absent" => absent.push(text),
                    _ => version = Some(text),
                }
            }
            _ if !arg.starts_with('-') && origin.is_none() => origin = Some(arg),
            _ => return Err(format!("Unexpected argument {arg}").into()),
        }
    }
    let input = origin.ok_or("Usage: cargo xtask check-served <origin> [--present <text>] [--absent <text>] [--expect-version <version>]")?;
    let invalid = "Expected an HTTP(S) origin, e.g. https://minarbot.example.";
    let origin = Url::parse(input).map_err(|_| invalid)?;
    if !matches!(origin.scheme(), "http" | "https")
        || !origin.username().is_empty()
        || origin.password().is_some()
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
    {
        return Err(invalid.into());
    }
    let client = Client::builder()
        .redirect(Policy::none())
        .timeout(Duration::from_secs(15))
        .build()?;
    let mut report = Vec::new();
    let html = get(
        &client,
        &origin,
        &origin,
        |mime| mime.to_ascii_lowercase().contains("text/html"),
        &mut report,
    )?;
    let mut assets = Vec::new();
    // ponytail: Vite emits static tags. Use an HTML parser if it emits dynamic markup.
    for tag in TAGS.captures_iter(&COMMENTS.replace_all(&html, "")) {
        let attribute = if tag[1].eq_ignore_ascii_case("script") {
            "src"
        } else {
            "href"
        };
        let mut reference = None;
        for attr in ATTRS.captures_iter(&tag[0]) {
            if attr[1].eq_ignore_ascii_case(attribute) {
                reference =
                    (2..=4).find_map(|index| attr.get(index).map(|text| text.as_str().to_owned()));
            }
        }
        if let Some(reference) = reference.filter(|text| !text.is_empty()) {
            let asset = origin.join(&reference.replace("&amp;", "&"))?;
            if ["js", "mjs", "css"]
                .iter()
                .any(|ext| extension(&asset).eq_ignore_ascii_case(ext))
                && !assets.contains(&asset)
            {
                assets.push(asset);
            }
        }
    }
    for (name, required) in [("JS", &["js", "mjs"][..]), ("CSS", &["css"][..])] {
        if !assets.iter().any(|asset| {
            required
                .iter()
                .any(|ext| extension(asset).eq_ignore_ascii_case(ext))
        }) {
            return Err(
                format!("index.html references no {name} assets; check served build.").into(),
            );
        }
    }
    let mut files = vec![("/".to_owned(), html)];
    for asset in assets {
        let mime = |mime: &str| {
            if extension(&asset).eq_ignore_ascii_case("css") {
                mime.to_ascii_lowercase().contains("text/css")
            } else {
                SCRIPT.is_match(mime)
            }
        };
        files.push((
            asset.path().to_owned(),
            get(&client, &origin, &asset, mime, &mut report)?,
        ));
    }
    let status = |pass| if pass { "PASS" } else { "FAIL" };
    let mut ok = true;
    for (markers, wanted) in [(present, true), (absent, false)] {
        for marker in markers {
            let paths: Vec<&str> = files
                .iter()
                .filter(|(_, text)| text.contains(marker))
                .map(|(path, _)| path.as_str())
                .collect();
            let pass = paths.is_empty() != wanted;
            ok &= pass;
            let location = if paths.is_empty() {
                " (not found)".into()
            } else {
                format!(" in {}", paths.join(", "))
            };
            report.push(format!(
                "{} {} {}{location}",
                status(pass),
                if wanted { "present" } else { "absent" },
                serde_json::to_string(marker)?
            ));
        }
    }
    if let Some(version) = version {
        let body = get(
            &client,
            &origin,
            &origin.join("/api/updates")?,
            |mime| mime.to_ascii_lowercase().contains("application/json"),
            &mut report,
        )?;
        let value: Value =
            serde_json::from_str(&body).map_err(|error| format!("Invalid JSON: {error}"))?;
        let actual = value["current"]["version"].as_str();
        let pass = actual == Some(version);
        ok &= pass;
        report.push(format!(
            "{} version: expected {version}, served {}",
            status(pass),
            actual.unwrap_or("(missing current.version)")
        ));
    }
    for line in report {
        println!("{line}");
    }
    println!("Served check {}.", if ok { "passed" } else { "FAILED" });
    Ok(ok)
}

pub fn command(args: &[String]) -> Result<()> {
    match check(args) {
        Ok(true) => Ok(()),
        Ok(false) => Err("Served check FAILED.".into()),
        Err(error) => Err(format!("Served check FAILED: {error}").into()),
    }
}
