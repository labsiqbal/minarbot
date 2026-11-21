use super::{archive, release, Failure, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::{header, redirect::Policy, Client, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, future::Future, path::Path, time::Duration};

const GITHUB_REPOSITORY: &str = "labsiqbal/minarbot";
pub(super) const SOURCE: &str = "GitHub";
pub(super) const UNREACHABLE: &str = "github_unreachable";
pub(super) const INVALID: &str = "github_release_invalid";

pub(super) fn unreachable() -> Failure {
    Failure::new(502, UNREACHABLE, "GitHub is unreachable.")
}
pub(super) fn invalid(message: impl Into<String>) -> Failure {
    Failure::new(502, INVALID, message)
}
fn allowed(url: &Url, fixture_host: Option<&str>) -> bool {
    url.username().is_empty()
        && url.password().is_none()
        && match fixture_host {
            Some(host) => matches!(url.scheme(), "http" | "https") && url.host_str() == Some(host),
            None => {
                url.scheme() == "https"
                    && url.port().is_none()
                    && matches!(
                        url.host_str(),
                        Some(
                            "github.com"
                                | "objects.githubusercontent.com"
                                | "release-assets.githubusercontent.com"
                        )
                    )
            }
        }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Asset {
    size: u64,
    sha256: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    pub version: String,
    pub id: String,
    pub commit: String,
    pub archive: String,
    pub sha256: String,
    pub size: u64,
    pub notes: String,
    pub created: String,
    #[serde(default)]
    assets: BTreeMap<String, Asset>,
    #[serde(default)]
    pub computer_image: Option<String>,
}
#[derive(Deserialize, Serialize)]
struct SignedManifest {
    manifest: String,
    signature: String,
}

impl Manifest {
    fn verify(bytes: &[u8], signature: &[u8], key: &[u8; 32]) -> Result<Self> {
        let signature = STANDARD
            .decode(signature)
            .map_err(|_| invalid("Invalid GitHub release signature."))?;
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
            .verify(bytes, &signature)
            .map_err(|_| invalid("Invalid GitHub release signature."))?;
        let mut value: Self = serde_json::from_slice(bytes)
            .map_err(|error| invalid(format!("Invalid GitHub release manifest: {error}")))?;
        if !release::version(&value.version)
            || !release::id(&value.id)
            || !value.id.starts_with(&format!("{}-", value.version))
            || value.archive != format!("minarbot-{}-linux-x86_64.tar.gz", value.version)
        {
            return Err(invalid(
                "GitHub release version, ID or archive name mismatch.",
            ));
        }
        if value.commit.len() != 40
            || !value.commit.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !value
                .commit
                .to_ascii_lowercase()
                .starts_with(&value.id[value.version.len() + 1..])
        {
            return Err(invalid("Invalid GitHub release commit."));
        }
        if !archive::valid_hash(&value.sha256)
            || value.computer_image.as_deref().is_some_and(|digest| {
                !digest
                    .strip_prefix("sha256:")
                    .is_some_and(archive::valid_hash)
            })
            || !(1..=archive::LIMIT).contains(&value.size)
            || value.notes.encode_utf16().count() > 16_000
            || !chrono::DateTime::parse_from_rfc3339(&value.created)
                .is_ok_and(|date| date.offset().local_minus_utc() == 0)
        {
            return Err(invalid(
                "Invalid GitHub release archive metadata, notes or creation date.",
            ));
        }
        let name = format!(
            "minarbot-{}-{}-{}.tar.gz",
            value.version,
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        if let Some(asset) = value.assets.remove(&name) {
            if !archive::valid_hash(&asset.sha256) || !(1..=archive::LIMIT).contains(&asset.size) {
                return Err(invalid("Invalid platform archive metadata."));
            }
            value.archive = name;
            value.sha256 = asset.sha256;
            value.size = asset.size;
        } else if !value.assets.is_empty() || name != value.archive {
            return Err(invalid("GitHub release is unavailable on this platform."));
        }
        value.assets.clear();
        Ok(value)
    }
    pub(super) fn public(&self) -> Value {
        json!({"id":self.id,"version":self.version,"commit":self.commit,"notes":self.notes,"created":self.created,"source":SOURCE})
    }
    pub(super) fn matches(&self, installed: &Value) -> bool {
        installed["id"] == self.id
            && installed["version"] == self.version
            && installed["commit"]
                .as_str()
                .is_some_and(|commit| commit.eq_ignore_ascii_case(&self.commit))
    }
}

type Cached = Option<(tokio::time::Instant, Result<Manifest>)>;

pub(super) struct Feed {
    base: String,
    client: Client,
    key: [u8; 32],
    cached: tokio::sync::Mutex<Cached>,
}
impl Feed {
    pub(super) fn new(base: Option<&str>, key: Option<&str>) -> crate::db::Result<Self> {
        let releases = format!("https://github.com/{GITHUB_REPOSITORY}/releases");
        let url = Url::parse(base.unwrap_or(&releases))?;
        let fixture_host = base.and_then(|_| url.host_str().map(str::to_owned));
        if (base.is_some()
            && !matches!(
                fixture_host.as_deref(),
                Some("127.0.0.1" | "[::1]" | "localhost")
            ))
            || !allowed(&url, fixture_host.as_deref())
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("Invalid GitHub release base URL.".into());
        }
        let key = match key {
            Some(key) => STANDARD
                .decode(key)?
                .try_into()
                .map_err(|_| "Invalid test release public key")?,
            None => minarbot_launcher::RELEASE_PUBLIC_KEY,
        };
        let client = Client::builder()
            .no_proxy()
            .redirect(Policy::custom(move |attempt| {
                if attempt.previous().len() < 4 && allowed(attempt.url(), fixture_host.as_deref()) {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            }))
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self {
            base: url.as_str().trim_end_matches('/').to_owned(),
            client,
            key,
            cached: Default::default(),
        })
    }
    async fn response(&self, url: Url, range: Option<u64>) -> Result<Response> {
        let mut request = self.client.get(url);
        if let Some(offset) = range.filter(|offset| *offset > 0) {
            request = request.header(header::RANGE, format!("bytes={offset}-"));
        }
        let response = request.send().await.map_err(|_| unreachable())?;
        if response.status().is_redirection() {
            return Err(invalid(
                "GitHub release download redirected to a disallowed URL.",
            ));
        }
        if response.status().is_server_error() {
            return Err(unreachable());
        }
        Ok(response)
    }
    async fn asset(&self, release: &str, name: &str, limit: usize) -> Result<String> {
        let url = Url::parse(&format!("{}/{release}/{name}", self.base))
            .map_err(|_| invalid("Invalid GitHub release URL."))?;
        let mut response = self.response(url, None).await?;
        if !response.status().is_success() {
            return Err(if name.ends_with(".sig") {
                invalid("GitHub release signature is missing.")
            } else {
                invalid("GitHub release manifest is unavailable.")
            });
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| unreachable())? {
            if bytes.len() + chunk.len() > limit {
                return Err(invalid("GitHub release metadata exceeds size limit."));
            }
            bytes.extend_from_slice(&chunk);
        }
        String::from_utf8(bytes).map_err(|_| invalid("Invalid GitHub release asset encoding."))
    }
    async fn signed(
        &self,
        release: &str,
        manifest: &str,
        signature: &str,
    ) -> Result<SignedManifest> {
        let (manifest, signature) = tokio::join!(
            self.asset(release, manifest, 128 * 1024),
            self.asset(release, signature, 88)
        );
        Ok(SignedManifest {
            manifest: manifest?,
            signature: signature?,
        })
    }
    fn verify(&self, signed: &SignedManifest) -> Result<Manifest> {
        Manifest::verify(
            signed.manifest.as_bytes(),
            signed.signature.as_bytes(),
            &self.key,
        )
    }
    pub(super) async fn computer_image(&self, id: &str, cache: &Path) -> Result<Option<String>> {
        let (version, _) = id
            .rsplit_once('-')
            .filter(|_| release::id(id))
            .ok_or_else(|| invalid("Computer image needs an installed Minarbot release."))?;
        if let Some(manifest) = std::fs::read(cache)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<SignedManifest>(&bytes).ok())
            .and_then(|signed| self.verify(&signed).ok())
            .filter(|manifest| manifest.id == id)
        {
            return Ok(manifest.computer_image);
        }
        let tag = format!("download/v{version}");
        let signed = tokio::time::timeout(
            Duration::from_secs(15),
            self.signed(&tag, "computer-image.json", "computer-image.json.sig"),
        )
        .await
        .map_err(|_| unreachable())??;
        let manifest = self.verify(&signed)?;
        if manifest.id != id {
            return Err(invalid(
                "Computer image manifest does not match the installed release.",
            ));
        }
        let bytes = serde_json::to_vec(&signed).map_err(|_| Failure::storage())?;
        minarbot_launcher::files::durable(cache, &bytes).map_err(|_| Failure::storage())?;
        Ok(manifest.computer_image)
    }
    async fn latest(&self) -> Result<Manifest> {
        self.verify(
            &self
                .signed("latest/download", "latest.json", "latest.json.sig")
                .await?,
        )
    }
    pub(super) async fn check(&self) -> Result<Manifest> {
        let mut cached = self.cached.lock().await;
        if let Some((at, result)) = cached.as_ref() {
            if at.elapsed() < Duration::from_secs(15) {
                return result.clone();
            }
        }
        let result = tokio::time::timeout(Duration::from_secs(6), self.latest())
            .await
            .unwrap_or_else(|_| Err(unreachable()));
        *cached = Some((tokio::time::Instant::now(), result.clone()));
        result
    }

    pub(super) async fn stage<A: Future<Output = Result<()>>>(
        &self,
        root: &Path,
        manifest: &Manifest,
        authorize: impl FnOnce() -> A,
    ) -> Result<u64> {
        let url = Url::parse(&format!(
            "{}/download/v{}/{}",
            self.base, manifest.version, manifest.archive
        ))
        .map_err(|_| invalid("Invalid GitHub release archive URL."))?;
        archive::receive(
            root,
            archive::Download::GitHub(manifest),
            |offset| {
                let url = url.clone();
                async move {
                    let response = self.response(url, Some(offset)).await?;
                    if !matches!(
                        response.status(),
                        StatusCode::OK
                            | StatusCode::PARTIAL_CONTENT
                            | StatusCode::RANGE_NOT_SATISFIABLE
                    ) {
                        return Err(invalid("GitHub release archive is unavailable."));
                    }
                    Ok(response)
                }
            },
            authorize,
        )
        .await
    }
}

pub(crate) fn remote_error(code: &str, message: &str) -> Option<Failure> {
    match code {
        UNREACHABLE => Some(unreachable()),
        INVALID => Some(invalid(match message {
            "Invalid GitHub release signature."
            | "GitHub release signature is missing."
            | "GitHub release SHA-256 mismatch."
            | "GitHub release archive size mismatch or truncated archive."
            | "GitHub release archive download was interrupted or truncated."
            | "GitHub release download redirected to a disallowed URL." => message,
            _ => "Receiving machine rejected the signed GitHub release.",
        })),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_downloads_require_https_and_allow_only_the_release_hosts() -> crate::db::Result<()> {
        for host in [
            "github.com",
            "objects.githubusercontent.com",
            "release-assets.githubusercontent.com",
        ] {
            assert!(allowed(
                &Url::parse(&format!("https://{host}/asset"))?,
                None
            ));
            assert!(!allowed(
                &Url::parse(&format!("http://{host}/asset"))?,
                None
            ));
        }
        for url in [
            "https://api.github.com/asset",
            "https://github.com.example.test/asset",
            "https://other.example.test@github.com/asset",
            "https://github.com:8443/asset",
            "http://127.0.0.1/asset",
        ] {
            assert!(!allowed(&Url::parse(url)?, None), "{url}");
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/github_redirect.rs"]
mod redirect_tests;
