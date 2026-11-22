// CSP adapted from T3 Code (MIT, app/t3/NOTICE).
use super::*;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

const PROXY: &str = include_str!("proxy.html");
const CLOSE_ON_NAVIGATION: &str = r#"<script>
addEventListener('pagehide', () => parent.postMessage({ minarbotAppUnloaded: true }, '*'), { once: true });
</script>"#;
const SOURCE: &str =
    r"(?i)^(?:https?|wss?)://(?:\*\.)?[a-z0-9-]+(?:\.[a-z0-9-]+)*(?::[0-9]{1,5})?$";
static HOST_SOURCE: LazyLock<Option<regex::Regex>> =
    LazyLock::new(|| regex::Regex::new(SOURCE).ok());

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Csp {
    connect_domains: Vec<String>,
    resource_domains: Vec<String>,
    frame_domains: Vec<String>,
    base_uri_domains: Vec<String>,
}

impl Csp {
    pub(super) fn read(state: &AppState, meta: &Value) -> Self {
        let own_hosts = std::iter::once(&state.origin)
            .chain(&state.config.allowed_origins)
            .filter_map(|origin| url::Url::parse(origin).ok())
            .filter_map(|url| url.host_str().map(str::to_owned))
            .collect::<Vec<_>>();
        let domains = |key: &str| {
            meta[key]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter_map(|source| allowed_source(source, &own_hosts))
                .take(16)
                .collect()
        };
        Self {
            connect_domains: domains("connectDomains"),
            resource_domains: domains("resourceDomains"),
            frame_domains: domains("frameDomains"),
            base_uri_domains: domains("baseUriDomains"),
        }
    }

    fn policy(&self) -> String {
        let resources = self.resource_domains.join(" ");
        let sources = |domains: &[String]| {
            if domains.is_empty() {
                "'none'".into()
            } else {
                domains.join(" ")
            }
        };
        format!(
            "default-src 'none'; script-src 'unsafe-inline' {resources}; style-src 'unsafe-inline' {resources}; \
             img-src data: blob: {resources}; font-src data: {resources}; media-src data: blob: {resources}; \
             connect-src {}; frame-src {}; base-uri {}; form-action 'none'; object-src 'none'; worker-src 'none'",
            sources(&self.connect_domains), sources(&self.frame_domains), sources(&self.base_uri_domains),
        )
    }
}

fn allowed_source(source: &str, own_hosts: &[String]) -> Option<String> {
    if source.len() > 256 || !HOST_SOURCE.as_ref()?.is_match(source) {
        return None;
    }
    let normalized = source.to_ascii_lowercase();
    let wildcard = normalized.contains("://*.");
    let parsed = url::Url::parse(&normalized.replacen("://*.", "://", 1)).ok()?;
    // IP literals and local names are unnecessary for declared public web origins.
    let url::Host::Domain(host) = parsed.host()? else {
        return None;
    };
    if !host.contains('.')
        || [".localhost", ".local", ".internal"]
            .iter()
            .any(|suffix| host.ends_with(suffix))
        || own_hosts.iter().any(|own| {
            own == host
                || wildcard
                    && own
                        .strip_suffix(host)
                        .is_some_and(|prefix| prefix.ends_with('.'))
        })
    {
        return None;
    }
    Some(normalized)
}

fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

pub(super) fn serve(html: &str, csp: &Csp) -> Result<Response> {
    if html.len() > MAX_HTML {
        return Err(Failure::storage());
    }
    let policy = csp.policy();
    let app = format!("<!doctype html><meta charset=utf-8><meta http-equiv=\"Content-Security-Policy\" content=\"{}\">{CLOSE_ON_NAVIGATION}{html}", escape_attribute(&policy));
    let proxy = PROXY.replace("{{APP_DOCUMENT}}", &escape_attribute(&app));
    crate::files::rich_html::sandboxed_document(proxy, &policy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csp_origin_sources_are_bounded_and_exclude_the_host() -> Result<()> {
        let own = vec!["chat.example.com".into()];
        for source in [
            "'self'",
            "https:",
            "https://example.com;script-src",
            "https://example.com/foo",
            "https://example.com\n",
            "https://chat.example.com",
            "https://*.example.com",
            "https://127.1:4317",
            "http://foo.local",
            "https://ex.com:99999",
        ] {
            assert!(allowed_source(source, &own).is_none(), "{source}");
        }
        assert_eq!(
            allowed_source("https://*.ASSETS.example:443", &own).as_deref(),
            Some("https://*.assets.example:443")
        );
        Ok(())
    }
}
