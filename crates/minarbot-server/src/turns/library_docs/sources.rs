use super::{stack, Ecosystem, Lookup};
use crate::turns::web::{self, FetchPolicy};
use reqwest::StatusCode;
use serde_json::Value;
use std::{
    sync::atomic::{AtomicI64, Ordering},
    time::Duration,
};
use url::Url;

pub const CONTEXT7: &str = "context7.com";
const ATTEMPT: Duration = Duration::from_secs(5);
const DOCS_LIMIT: usize = 12_000;

#[derive(Default)]
pub(super) struct Sources {
    pub(super) policy: FetchPolicy,
    pub(super) request: tokio::sync::Mutex<()>,
    backoff_until: AtomicI64,
    #[cfg(test)]
    pub(super) fixture: Option<std::net::SocketAddr>,
}

#[derive(serde::Serialize)]
pub(super) struct Docs {
    pub(super) source: &'static str,
    pub(super) url: String,
    pub(super) content: String,
    pub(super) source_version: Option<String>,
}

impl Sources {
    fn target(&self, url: &Url) -> Url {
        #[cfg(test)]
        if let Some(socket) = self.fixture {
            if let Ok(mut proxied) = Url::parse(&format!(
                "http://{socket}/{}{}",
                url.host_str().unwrap_or(""),
                url.path()
            )) {
                proxied.set_query(url.query());
                return proxied;
            }
        }
        url.clone()
    }

    async fn context_response(&self, url: &Url, key: Option<&str>, now: i64) -> Option<Vec<u8>> {
        if self.backoff_until.load(Ordering::Relaxed) > now {
            return None;
        }
        tokio::time::timeout(ATTEMPT, async {
            // The fixed host is the only recipient of the Vault key. No redirects are followed.
            let response = web::get(&self.target(url), &self.policy, key).await.ok()?;
            if response.status() == StatusCode::TOO_MANY_REQUESTS {
                let delay = response
                    .headers()
                    .get("retry-after")
                    .and_then(|h| h.to_str().ok())
                    .and_then(|s| {
                        s.parse::<i64>().ok().or_else(|| {
                            chrono::DateTime::parse_from_rfc2822(s)
                                .ok()
                                .map(|date| date.timestamp() - now / 1000)
                        })
                    })
                    .unwrap_or(60)
                    .max(1);
                self.backoff_until.fetch_max(
                    now.saturating_add(delay.saturating_mul(1000)),
                    Ordering::Relaxed,
                );
            }
            if response.status() != StatusCode::OK {
                return None;
            }
            web::body(response).await.ok()
        })
        .await
        .ok()
        .flatten()
    }

    pub(super) async fn reachable(&self, key: Option<&str>, now: i64) -> bool {
        let Some(mut url) = origin(CONTEXT7, "/api/v2/libs/search") else {
            return false;
        };
        url.query_pairs_mut().append_pair("libraryName", "react");
        self.context_response(&url, key, now)
            .await
            .is_some_and(|bytes| {
                serde_json::from_slice::<Value>(&bytes).is_ok_and(|v| v["results"].is_array())
            })
    }

    pub(super) async fn context7(
        &self,
        lookup: &Lookup,
        key: Option<&str>,
        now: i64,
    ) -> Option<Docs> {
        let mut url = origin(CONTEXT7, "/api/v2/libs/search")?;
        url.query_pairs_mut()
            .append_pair("libraryName", &lookup.name)
            .append_pair("query", &lookup.topic);
        let search: Value =
            serde_json::from_slice(&self.context_response(&url, key, now).await?).ok()?;
        let needle = lookup.name.trim_start_matches('@');
        let short_name = needle
            .rsplit(['/', ':'])
            .find(|part| !part.starts_with('v') || !part[1..].bytes().all(|b| b.is_ascii_digit()))
            .unwrap_or(needle);
        let (_, id, selected) = search["results"]
            .as_array()?
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let id = row["id"].as_str()?;
                let path = id.strip_prefix('/')?;
                if !stack::package_name(path)
                    || path.split('/').count() != 2
                    || path.contains([':', '@'])
                    || row["state"].as_str().is_some_and(|s| s != "finalized")
                {
                    return None;
                }
                let name = path.split_once('/')?.1;
                let title = row["title"].as_str().unwrap_or("");
                let exact = path.eq_ignore_ascii_case(needle)
                    || needle.strip_prefix("github.com/").is_some_and(|repo| {
                        repo == path
                            || repo
                                .strip_prefix(path)
                                .is_some_and(|suffix| suffix.starts_with("/v"))
                    });
                let matches = exact
                    || name.eq_ignore_ascii_case(short_name)
                    || title.eq_ignore_ascii_case(short_name);
                matches.then_some(((exact, std::cmp::Reverse(index)), id, row))
            })
            .max_by_key(|(score, _, _)| *score)?;
        let mut url = origin(CONTEXT7, "/api/v2/context")?;
        let version = lookup.version.as_deref().map(|version| {
            selected["versions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .find(|tag| tag.trim_start_matches('v') == version.trim_start_matches('v'))
                .unwrap_or(version)
        });
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("libraryId", id)
                .append_pair("query", &lookup.topic)
                .append_pair("type", "txt");
            if let Some(version) = version {
                query.append_pair("version", version);
            }
        }
        let bytes = self.context_response(&url, key, now).await?;
        let text = std::str::from_utf8(&bytes).ok()?.trim();
        if text.is_empty() || text.starts_with('{') || text.starts_with("Documentation not found") {
            return None;
        }
        // Context7 explicitly reports closest indexed versions in its text response.
        if text.starts_with("Version note:") {
            return None;
        }
        let page = origin(CONTEXT7, id)?;
        Some(Docs {
            source: "Context7",
            url: page.into(),
            content: clipped(text),
            source_version: lookup.version.clone(),
        })
    }

    async fn page(&self, url: &Url) -> Option<Value> {
        let response = tokio::time::timeout(
            ATTEMPT,
            web::fetch_text(self.target(url).as_str(), self.policy, 192_000),
        )
        .await
        .ok()?
        .ok()?;
        if response["text"].as_str()?.trim().is_empty() {
            return None;
        }
        Some(response)
    }

    async fn json(&self, url: &Url) -> Option<Value> {
        tokio::time::timeout(ATTEMPT, web::fetch_json(&self.target(url), &self.policy))
            .await
            .ok()?
            .ok()
    }

    pub(super) async fn official(&self, lookup: &Lookup) -> Option<Docs> {
        let name = &lookup.name;
        let version = lookup.version.as_deref();
        let ecosystem = lookup.ecosystem?;
        let selected_version = version.unwrap_or("latest");
        let version_segment = segment(selected_version);
        let package = segment(name);
        let (source, registry) = match ecosystem {
            Ecosystem::Rust => (
                "docs.rs",
                origin("docs.rs", &format!("/crate/{package}/{version_segment}"))?,
            ),
            Ecosystem::Go => (
                "pkg.go.dev",
                origin(
                    "pkg.go.dev",
                    &format!(
                        "/{name}{}",
                        version
                            .map(|v| format!("@{}", segment(v)))
                            .unwrap_or_default()
                    ),
                )?,
            ),
            Ecosystem::Npm => (
                "npm",
                origin(
                    "registry.npmjs.org",
                    &format!("/{package}/{version_segment}"),
                )?,
            ),
            Ecosystem::Python => (
                "PyPI",
                origin(
                    "pypi.org",
                    &format!(
                        "/pypi/{package}{}/json",
                        version
                            .map(|_| format!("/{version_segment}"))
                            .unwrap_or_default()
                    ),
                )?,
            ),
            Ecosystem::Ruby => (
                "RubyGems",
                origin(
                    "rubygems.org",
                    &version.map_or_else(
                        || format!("/api/v1/gems/{package}.json"),
                        |_| format!("/api/v2/rubygems/{package}/versions/{version_segment}.json"),
                    ),
                )?,
            ),
            Ecosystem::Php => (
                "Packagist",
                origin("repo.packagist.org", &format!("/p2/{name}.json"))?,
            ),
            Ecosystem::Maven => {
                let (group, artifact) = name.split_once(':')?;
                return self.maven(group, artifact, version).await;
            }
            Ecosystem::Dart => (
                "pub.dev",
                origin(
                    "pub.dev",
                    &format!(
                        "/api/packages/{package}{}",
                        version
                            .map(|_| format!("/versions/{version_segment}"))
                            .unwrap_or_default()
                    ),
                )?,
            ),
            Ecosystem::Swift => (
                "Swift Package Index",
                origin("swiftpackageindex.com", &format!("/{name}"))?,
            ),
            Ecosystem::Nuget => (
                "NuGet",
                origin(
                    "www.nuget.org",
                    &format!(
                        "/packages/{package}{}",
                        version
                            .map(|_| format!("/{version_segment}"))
                            .unwrap_or_default()
                    ),
                )?,
            ),
        };
        if matches!(
            ecosystem,
            Ecosystem::Rust | Ecosystem::Go | Ecosystem::Swift | Ecosystem::Nuget
        ) {
            let target = if ecosystem == Ecosystem::Rust {
                origin("docs.rs", &format!("/{package}/{version_segment}"))?
            } else {
                registry.clone()
            };
            let page = self.page(&target).await;
            let page = if page.is_none() && ecosystem == Ecosystem::Rust {
                self.page(&registry).await
            } else {
                page
            };
            if let Some(page) = page {
                let fetched = page["url"]
                    .as_str()
                    .and_then(|v| Url::parse(v).ok())
                    .unwrap_or(target);
                let mut url = Url::parse(&self.display_url(&fetched, &registry)).ok()?;
                let text = page["text"].as_str()?;
                if ecosystem == Ecosystem::Go {
                    if let Some(symbol) = lookup.topic.split_whitespace().next().filter(|symbol| {
                        symbol.starts_with(|c: char| c.is_ascii_uppercase())
                            && symbol
                                .bytes()
                                .all(|c| c.is_ascii_alphanumeric() || c == b'_')
                            && text.contains(&format!("func {symbol}"))
                    }) {
                        url.set_fragment(Some(symbol));
                    }
                }
                let source_version = version.filter(|v| {
                    ecosystem != Ecosystem::Swift
                        && url.path().split('/').any(|segment| {
                            segment == *v
                                || segment
                                    .rsplit_once('@')
                                    .is_some_and(|(_, version)| version == *v)
                        })
                });
                return Some(Docs {
                    source,
                    url: url.into(),
                    content: excerpt(text, &lookup.topic),
                    source_version: source_version.map(str::to_owned),
                });
            }
            return self.official_site(known_site(lookup)?, lookup).await;
        }
        let mut fallback = None;
        let mut sites = Vec::new();
        if let Some(value) = self.json(&registry).await {
            let meta = match ecosystem {
                Ecosystem::Python => Some(&value["info"]),
                Ecosystem::Php => value["packages"][name].as_array().and_then(|rows| {
                    rows.iter().find(|row| {
                        version.is_none_or(|v| {
                            row["version"].as_str().is_some_and(|tag| {
                                tag.trim_start_matches('v') == v.trim_start_matches('v')
                            })
                        })
                    })
                }),
                Ecosystem::Dart => Some(
                    value
                        .get("pubspec")
                        .or_else(|| value.get("latest").and_then(|v| v.get("pubspec")))?,
                ),
                _ => Some(&value),
            };
            if let Some(meta) = meta.filter(|m| m.is_object()) {
                for candidate in [
                    &meta["documentation"],
                    &meta["documentation_uri"],
                    &meta["project_urls"]["Documentation"],
                    &meta["project_urls"]["Docs"],
                    &meta["homepage"],
                    &meta["home_page"],
                    &meta["homepage_uri"],
                    &meta["project_urls"]["Homepage"],
                    &meta["repository"]["url"],
                    &meta["source_code_uri"],
                    &meta["repository"],
                    &meta["source"]["url"],
                ] {
                    if let Some(site) = candidate.as_str().and_then(public_site) {
                        if !sites.contains(&site) {
                            sites.push(site);
                        }
                    }
                }
                let content = ["readme", "description", "info", "summary"]
                    .iter()
                    .filter_map(|k| meta[k].as_str())
                    .filter(|s| !s.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let source_version = meta["version"]
                    .as_str()
                    .or(meta["number"].as_str())
                    .or_else(|| value["version"].as_str())
                    .filter(|v| stack::exact(v).is_some())
                    .map(str::to_owned)
                    .or(lookup.version.clone());
                if !content.is_empty() {
                    fallback = Some(Docs {
                        source,
                        url: registry.to_string(),
                        content: excerpt(&content, &lookup.topic),
                        source_version,
                    });
                }
                if fallback.as_ref().is_some_and(|docs| {
                    docs.content.len() >= 350 && topic_score(&docs.content, &lookup.topic) > 0
                }) {
                    return fallback;
                }
            }
        }
        if let Some(site) = known_site(lookup) {
            if !sites.contains(&site) {
                sites.push(site);
            }
        }
        // Registry metadata is a last useful result when the docs site has no readable page.
        for site in sites.into_iter().take(2) {
            if let Some(docs) = self.official_site(site, lookup).await {
                return Some(docs);
            }
        }
        fallback
    }

    async fn maven(&self, group: &str, artifact: &str, version: Option<&str>) -> Option<Docs> {
        let path = format!(
            "/doc/{}/{}/{}",
            segment(group),
            segment(artifact),
            segment(version.unwrap_or("latest"))
        );
        let url = origin("javadoc.io", &path)?;
        let page = self.page(&url).await?;
        Some(Docs {
            source: "javadoc.io",
            url: url.into(),
            content: clipped(page["text"].as_str()?),
            source_version: version.map(str::to_owned),
        })
    }

    async fn official_site(&self, site: Url, lookup: &Lookup) -> Option<Docs> {
        let mut targets = Vec::new();
        if let Some(root) = origin(site.host_str()?, "/") {
            for file in ["llms.txt", "llms-full.txt"] {
                if let Ok(url) = root.join(file) {
                    targets.push((file, url));
                }
            }
        }
        targets.push(("Official documentation", site));
        let mut weak = None;
        for (source, url) in targets {
            if let Some(page) = self.page(&url).await {
                let text = page["text"].as_str()?;
                if source != "Official documentation"
                    && (!text.trim_start().starts_with('#') || text.contains("<html"))
                {
                    continue;
                }
                let final_url = Url::parse(page["url"].as_str()?).ok()?;
                let docs = Docs {
                    source,
                    url: self.display_url(&final_url, &url),
                    content: excerpt(text, &lookup.topic),
                    source_version: None,
                };
                if topic_score(text, &lookup.topic) > 0 || source == "Official documentation" {
                    return Some(docs);
                }
                weak.get_or_insert(docs);
            }
        }
        weak
    }

    fn display_url(&self, final_url: &Url, requested: &Url) -> String {
        if self.target(requested) == *requested {
            final_url.to_string()
        } else {
            requested.to_string()
        }
    }
}

fn origin(host: &str, path: &str) -> Option<Url> {
    Url::parse(&format!("https://{host}{path}")).ok()
}

fn segment(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn public_site(value: &str) -> Option<Url> {
    let mut url = Url::parse(value.strip_prefix("git+").unwrap_or(value)).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return None;
    }
    let path = url.path().trim_end_matches(".git").to_owned();
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    Some(url)
}

fn known_site(lookup: &Lookup) -> Option<Url> {
    let host = match lookup.name.as_str() {
        "react" | "react-dom" => "react.dev",
        "next" | "next.js" => "nextjs.org",
        "vue" => "vuejs.org",
        "svelte" => "svelte.dev",
        "tokio" => "tokio.rs",
        "django" => "docs.djangoproject.com",
        "flask" => "flask.palletsprojects.com",
        "rails" => "guides.rubyonrails.org",
        "symfony/symfony" => "symfony.com",
        _ => return None,
    };
    origin(host, "/")
}

fn clipped(text: &str) -> String {
    text.trim()[..text
        .trim()
        .floor_char_boundary(text.trim().len().min(DOCS_LIMIT))]
        .to_owned()
}

fn topic_score(text: &str, topic: &str) -> usize {
    let text = text.to_ascii_lowercase();
    topic
        .split_whitespace()
        .filter(|word| word.len() > 2 && text.contains(&word.to_ascii_lowercase()))
        .count()
}

fn excerpt(text: &str, topic: &str) -> String {
    if text.len() <= DOCS_LIMIT / 2 {
        return clipped(text);
    }
    let lines: Vec<_> = text.lines().collect();
    let mut matches: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let score = topic_score(line, topic);
            (score > 0).then_some((std::cmp::Reverse(score), index))
        })
        .collect();
    if matches.is_empty() {
        return clipped(text);
    }
    matches.sort_unstable();
    let ranges: std::collections::BTreeSet<_> = matches
        .into_iter()
        .take(14)
        .flat_map(|(_, i)| i.saturating_sub(2)..(i + 6).min(lines.len()))
        .collect();
    clipped(
        &ranges
            .into_iter()
            .map(|i| lines[i])
            .collect::<Vec<_>>()
            .join("\n"),
    )
}
