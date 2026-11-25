use super::{error, github, release, PeerError, Result};
use flate2::read::MultiGzDecoder;
use reqwest::StatusCode;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    future::Future,
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Weak},
    time::Duration,
};
pub(super) const LIMIT: u64 = 1024 * 1024 * 1024;
const EXPANDED: u64 = 2 * LIMIT;
pub struct Temporary(pub PathBuf);
impl Temporary {
    fn new(root: &Path, prefix: &str) -> Result<Self> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)
            .map_err(|_| error(500, "Could not prepare release."))?;
        let path = root.join(format!(".{prefix}-{}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| error(500, "Could not prepare release."))?;
        Ok(Self(path))
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
pub struct Archive {
    pub _directory: Temporary,
    pub file: PathBuf,
    pub sha256: String,
    pub size: u64,
}
fn hash(file: &Path) -> Result<String> {
    let mut file = File::open(file).map_err(|_| error(500, "Could not read release."))?;
    let mut hash = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        let n = file
            .read(&mut bytes)
            .map_err(|_| error(500, "Could not read release."))?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn preflight(path: &Path, total: &mut u64, entries: &mut usize) -> Result<()> {
    for entry in fs::read_dir(path).map_err(|_| error(500, "Could not read release."))? {
        let entry = entry.map_err(|_| error(500, "Could not read release."))?;
        let s = fs::symlink_metadata(entry.path())
            .map_err(|_| error(500, "Could not read release."))?;
        *entries += 1;
        if *entries > 100000 {
            return Err(PeerError::SizeLimit.detail("Release exceeds file count limit."));
        }
        if s.is_dir() {
            preflight(&entry.path(), total, entries)?;
        } else if s.is_file() {
            *total += s.len();
            if *total > EXPANDED {
                return Err(PeerError::SizeLimit.detail("Expanded release exceeds size limit."));
            }
        } else {
            return Err(error(500, "Unsupported release entry."));
        }
    }
    Ok(())
}
pub async fn create(root: &Path, id: &str) -> Result<Arc<Archive>> {
    type Cached = Arc<tokio::sync::Mutex<Weak<Archive>>>;
    static ARCHIVES: LazyLock<tokio::sync::Mutex<HashMap<PathBuf, Cached>>> =
        LazyLock::new(Default::default);
    if !release::id(id) {
        return Err(error(400, "Invalid release ID."));
    }
    preflight(&root.join(id), &mut 0, &mut 0)?;
    let key = root.join(id);
    let entry = {
        let mut archives = ARCHIVES.lock().await;
        archives.retain(|_, entry| {
            Arc::strong_count(entry) > 1
                || entry
                    .try_lock()
                    .map_or(true, |archive| archive.strong_count() > 0)
        });
        archives.entry(key).or_default().clone()
    };
    let mut cached = entry.lock().await;
    if let Some(archive) = cached.upgrade() {
        return Ok(archive);
    }
    let archive = Arc::new(build(root, id).await?);
    *cached = Arc::downgrade(&archive);
    Ok(archive)
}
async fn build(root: &Path, id: &str) -> Result<Archive> {
    if !release::id(id) {
        return Err(error(400, "Invalid release ID."));
    }
    release::verify(root, id)?;
    let directory = Temporary::new(root, "archive")?;
    let file = directory.0.join("release.tar.gz");
    let mut child = tokio::process::Command::new("tar")
        .args(["--format=ustar", "-czf"])
        .arg(&file)
        .arg("-C")
        .arg(root.join(id))
        .arg(".")
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| error(500, "Could not package release."))?;
    let status = tokio::time::timeout(Duration::from_secs(120), child.wait())
        .await
        .map_err(|_| error(500, "Could not package release."))?
        .map_err(|_| error(500, "Could not package release."))?;
    if !status.success() {
        return Err(error(500, "Could not package release."));
    }
    let size = fs::metadata(&file)
        .map_err(|_| error(500, "Could not read release."))?
        .len();
    if size > LIMIT {
        return Err(PeerError::SizeLimit.detail("Release exceeds download limit."));
    }
    let sha256 = hash(&file)?;
    Ok(Archive {
        _directory: directory,
        file,
        sha256,
        size,
    })
}
fn number(bytes: &[u8]) -> Result<u64> {
    let text = text(bytes)?.trim();
    if text.is_empty() || !text.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        return Err(error(502, "Invalid tar number."));
    }
    u64::from_str_radix(text, 8).map_err(|_| error(502, "Invalid tar number."))
}
fn text(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes.split(|b| *b == 0).next().unwrap_or_default())
        .map_err(|_| error(502, "Unsafe tar entry."))
}
fn extract(archive: &Path, destination: &Path) -> Result<()> {
    let file = File::open(archive).map_err(|_| error(502, "Could not read peer release."))?;
    let mut stream = MultiGzDecoder::new(file).take(EXPANDED + 1);
    let mut total = 0;
    let mut entries = 0;
    let mut ended = false;
    let mut seen = HashSet::new();
    loop {
        let mut header = [0; 512];
        let n = stream
            .read(&mut header[..1])
            .map_err(|_| error(502, "Invalid tar archive."))?;
        if n == 0 {
            if !ended {
                return Err(error(502, "Truncated tar archive."));
            }
            break;
        }
        if total >= EXPANDED {
            return Err(PeerError::SizeLimit.detail("Expanded release exceeds size limit."));
        }
        stream
            .read_exact(&mut header[1..])
            .map_err(|_| error(502, "Truncated tar archive."))?;
        total += 512;
        if total > EXPANDED {
            return Err(PeerError::SizeLimit.detail("Expanded release exceeds size limit."));
        }
        if header.iter().all(|b| *b == 0) {
            ended = true;
            continue;
        }
        entries += 1;
        if entries > 100000 {
            return Err(PeerError::SizeLimit.detail("Release exceeds file count limit."));
        }
        if ended {
            return Err(error(502, "Invalid tar trailer."));
        }
        let checksum: u64 = header
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    u64::from(*b)
                }
            })
            .sum();
        if checksum != number(&header[148..156])? {
            return Err(error(502, "Invalid tar header checksum."));
        }
        let prefix = text(&header[345..500])?;
        let raw = if prefix.is_empty() {
            text(&header[..100])?.to_owned()
        } else {
            format!("{prefix}/{}", text(&header[..100])?)
        };
        let name = raw.strip_prefix("./").unwrap_or(&raw);
        let name = name.strip_suffix('/').unwrap_or(name);
        let kind = header[156];
        let size = number(&header[124..136])?;
        let mode = number(&header[100..108])?;
        if raw == "./" && kind == b'5' {
            continue;
        }
        if name.is_empty()
            || name.starts_with('/')
            || name.chars().any(|c| c == '\\' || c.is_control())
            || name
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
            || !seen.insert(name.to_owned())
            || ![0, b'0', b'5'].contains(&kind)
            || kind == b'5' && size != 0
        {
            return Err(error(502, "Unsafe tar entry."));
        }
        if size > EXPANDED - total {
            return Err(PeerError::SizeLimit.detail("Expanded release exceeds size limit."));
        }
        let path = destination.join(name);
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(if kind == b'5' {
                &path
            } else {
                path.parent()
                    .ok_or_else(|| error(502, "Unsafe tar entry."))?
            })
            .map_err(|_| error(502, "Unsafe tar entry."))?;
        if kind != b'5' {
            let mut out = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(if mode & 0o111 != 0 { 0o700 } else { 0o600 })
                .open(&path)
                .map_err(|_| error(502, "Unsafe tar entry."))?;
            let mut left = size;
            let mut bytes = [0; 65536];
            while left > 0 {
                let n = left.min(bytes.len() as u64) as usize;
                stream
                    .read_exact(&mut bytes[..n])
                    .map_err(|_| error(502, "Truncated tar archive."))?;
                out.write_all(&bytes[..n]).map_err(|_| storage())?;
                left -= n as u64;
            }
            out.sync_all().map_err(|_| storage())?;
        }
        let padding = (512 - size % 512) % 512;
        let mut bytes = [0; 512];
        stream
            .read_exact(&mut bytes[..padding as usize])
            .map_err(|_| error(502, "Truncated tar archive."))?;
        total += size + padding;
        if total > EXPANDED {
            return Err(PeerError::SizeLimit.detail("Expanded release exceeds size limit."));
        }
    }
    Ok(())
}
const PARTIAL_PREFIX: &str = ".peer-release-partial-";
pub(super) fn partial_file(root: &Path, id: &str, expected: &str) -> PathBuf {
    let key = Sha256::digest(format!("{id}:{expected}"));
    root.join(format!("{PARTIAL_PREFIX}{key:x}"))
}

fn partial_path(root: &Path, id: &str, expected: &str) -> Result<PathBuf> {
    let partial = partial_file(root, id, expected);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(root)
        .map_err(|_| storage())?;
    // ponytail: keep one resumable archive per release directory. Budget multiple
    // partials if the receiver starts allowing parallel incoming updates.
    for entry in fs::read_dir(root).map_err(|_| storage())? {
        let entry = entry.map_err(|_| storage())?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.strip_prefix(PARTIAL_PREFIX).is_some_and(valid_hash))
            && entry.path() != partial
        {
            fs::remove_file(entry.path()).map_err(|_| storage())?;
        }
    }
    Ok(partial)
}

pub(super) fn valid_hash(expected: &str) -> bool {
    expected.len() == 64
        && expected
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn storage() -> crate::web::Failure {
    crate::web::Failure::new(500, "storage_failed", "Could not save peer release.")
}

fn metadata_error() -> crate::web::Failure {
    PeerError::IntegrityMismatch.detail("Invalid peer release metadata or byte range.")
}

fn resume_range(value: &str, offset: u64) -> Option<u64> {
    let (span, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = span.split_once('-')?;
    let total = total.parse::<u64>().ok()?;
    (offset > 0
        && start.parse::<u64>().ok()? == offset
        && end.parse::<u64>().ok()?.checked_add(1) == Some(total)
        && offset < total)
        .then_some(total)
}

async fn append(
    file: &mut File,
    mut response: reqwest::Response,
    expected: &str,
    expected_size: Option<u64>,
) -> Result<()> {
    let size_mismatch =
        || github::invalid("GitHub release archive size mismatch or truncated archive.");
    let mut offset = file.metadata().map_err(|_| storage())?.len();
    let status = response.status();
    if [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN].contains(&status) {
        return Err(PeerError::PermissionRevoked.failure());
    }
    if status == StatusCode::PAYLOAD_TOO_LARGE {
        return Err(PeerError::SizeLimit.failure());
    }
    if status == StatusCode::GATEWAY_TIMEOUT || status == StatusCode::REQUEST_TIMEOUT {
        return Err(PeerError::Timeout.failure());
    }
    let get = |key| response.headers().get(key).and_then(|v| v.to_str().ok());
    if !matches!(
        status,
        StatusCode::OK | StatusCode::PARTIAL_CONTENT | StatusCode::RANGE_NOT_SATISFIABLE
    ) {
        return Err(PeerError::ConnectionLost.detail(format!(
            "Origin rejected the release download (HTTP {status})."
        )));
    }
    if expected_size.is_none()
        && (get("x-minarbot-sha256") != Some(expected)
            || get("content-type") != Some("application/gzip"))
    {
        return Err(metadata_error());
    }
    let length = response.content_length();
    if length.is_some_and(|n| n > LIMIT) {
        return Err(PeerError::SizeLimit.failure());
    }
    let target = match status {
        StatusCode::OK => {
            file.set_len(0).map_err(|_| storage())?;
            offset = 0;
            if expected_size
                .zip(length)
                .is_some_and(|(expected, actual)| expected != actual)
            {
                return Err(size_mismatch());
            }
            expected_size.or(length)
        }
        StatusCode::PARTIAL_CONTENT => {
            let total = resume_range(get("content-range").unwrap_or(""), offset)
                .ok_or_else(metadata_error)?;
            if total > LIMIT {
                return Err(PeerError::SizeLimit.failure());
            }
            if expected_size.is_some_and(|size| size != total) {
                return Err(size_mismatch());
            }
            if length.is_some_and(|n| n != total - offset) {
                return Err(metadata_error());
            }
            Some(total)
        }
        // A process may stop just after writing the last byte. Verify the saved archive normally.
        _ if offset > 0 && get("content-range") == Some(format!("bytes */{offset}").as_str()) => {
            if expected_size.is_some_and(|size| size != offset) {
                return Err(size_mismatch());
            }
            return Ok(());
        }
        _ => return Err(metadata_error()),
    };
    while let Some(bytes) = response.chunk().await.map_err(PeerError::network)? {
        offset += bytes.len() as u64;
        if offset > LIMIT {
            return Err(PeerError::SizeLimit.failure());
        }
        if target.is_some_and(|total| offset > total) {
            return Err(metadata_error());
        }
        file.write_all(&bytes).map_err(|_| storage())?;
    }
    if target.is_some_and(|total| offset != total) {
        if expected_size.is_some() {
            return Err(size_mismatch());
        }
        return Err(PeerError::ConnectionLost.failure());
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub(super) enum Download<'a> {
    Peer { id: &'a str, sha256: &'a str },
    GitHub(&'a github::Manifest),
}

pub(super) async fn receive<D, A>(
    root: &Path,
    source: Download<'_>,
    mut download: impl FnMut(u64) -> D,
    authorize: impl FnOnce() -> A,
) -> Result<u64>
where
    D: Future<Output = Result<reqwest::Response>>,
    A: Future<Output = Result<()>>,
{
    let (id, expected, signed) = match source {
        Download::Peer { id, sha256 } => (id, sha256, None),
        Download::GitHub(manifest) => (
            manifest.id.as_str(),
            manifest.sha256.as_str(),
            Some(manifest),
        ),
    };
    let size = signed.map(|manifest| manifest.size);
    if !release::id(id) || !valid_hash(expected) {
        return Err(metadata_error());
    }
    let partial = partial_path(root, id, expected)?;
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(&partial)
        .map_err(|_| storage())?;
    minarbot_launcher::files::private_file(&partial).map_err(|_| storage())?;
    let mut zero_progress_attempts = 0;
    loop {
        let offset = file.metadata().map_err(|_| storage())?.len();
        let result = if offset > LIMIT {
            Err(PeerError::SizeLimit.failure())
        } else {
            match download(offset).await {
                Ok(response) => append(&mut file, response, expected, size).await,
                Err(failure) => Err(failure),
            }
        };
        match result {
            Ok(()) => break,
            Err(failure) => {
                let resumable = matches!(
                    failure.code,
                    "peer_transfer_timeout" | "peer_connection_lost" | github::UNREACHABLE
                );
                if !resumable {
                    fs::remove_file(&partial).map_err(|_| storage())?;
                    return Err(failure);
                }
                file.sync_all().map_err(|_| storage())?;
                let retained = file.metadata().map_err(|_| storage())?.len();
                zero_progress_attempts = if retained > offset {
                    0
                } else {
                    zero_progress_attempts + 1
                };
                if zero_progress_attempts >= 3 {
                    return Err(if signed.is_some() && failure.code != github::UNREACHABLE {
                        github::invalid(
                            "GitHub release archive download was interrupted or truncated.",
                        )
                    } else {
                        failure
                    });
                }
                eprintln!("peer release download retry release={id} bytes={retained} code={} message={:?}",
                    failure.code, failure.message);
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
    file.sync_all().map_err(|_| storage())?;
    let length = file.metadata().map_err(|_| storage())?.len();
    drop(file);
    let directory = Temporary::new(root, "download")?;
    let archive = partial.clone();
    let target = id.to_owned();
    let expected = expected.to_owned();
    let signed = signed.cloned();
    let destination = root.to_path_buf();
    let verified = async {
        let directory = tokio::task::spawn_blocking(move || {
            if hash(&archive)? != expected {
                return Err(if signed.is_some() {
                    github::invalid("GitHub release SHA-256 mismatch.")
                } else {
                    PeerError::IntegrityMismatch.detail("Peer release SHA-256 mismatch.")
                });
            }
            let stage = directory.0.join(&target);
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&stage)
                .map_err(|_| storage())?;
            extract(&archive, &stage).map_err(|failure| {
                if failure.code == "invalid_request" {
                    PeerError::IntegrityMismatch.detail(failure.message)
                } else {
                    failure
                }
            })?;
            let info = release::verify(&directory.0, &target).map_err(|_| {
                PeerError::IntegrityMismatch.detail("Release file manifest mismatch.")
            })?;
            if let Some(manifest) = signed {
                if !manifest.matches(&info) {
                    return Err(github::invalid(
                        "GitHub release version, ID or commit does not match the archive.",
                    ));
                }
                if fs::symlink_metadata(destination.join(&target)).is_ok() {
                    if release::verify(&destination, &target)
                        .is_ok_and(|installed| installed == info)
                    {
                        return Ok(None);
                    }
                    return Err(github::invalid(
                        "Installed release differs from the signed GitHub release.",
                    ));
                }
            }
            Ok(Some(directory))
        })
        .await
        .map_err(|_| storage())??;
        authorize().await?;
        Ok::<_, crate::web::Failure>(directory)
    }
    .await;
    fs::remove_file(&partial).map_err(|_| storage())?;
    if let Some(directory) = verified? {
        fs::rename(directory.0.join(id), root.join(id)).map_err(|_| storage())?;
        minarbot_launcher::files::sync_dir(root).map_err(|_| storage())?;
    }
    Ok(length)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, Bytes},
        http::{header, HeaderMap, Response, StatusCode},
        Router,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::task::JoinSet;

    const RELEASE_ID: &str = "1.2.3-abcdef12";

    async fn fixture_archive(root: &Path) -> Result<Arc<Archive>> {
        let release = root.join(RELEASE_ID);
        fs::create_dir(&release).map_err(|_| storage())?;
        fs::write(release.join("package.json"), br#"{"version":"1.2.3"}"#)
            .map_err(|_| storage())?;
        let files = minarbot_launcher::files::manifest(&release).map_err(|_| storage())?;
        let info = serde_json::json!({"id": RELEASE_ID, "version": "1.2.3", "files": files});
        fs::write(release.join("release.json"), info.to_string()).map_err(|_| storage())?;
        create(root, RELEASE_ID).await
    }

    async fn fixture_sender(
        tasks: &mut JoinSet<std::io::Result<()>>,
        archive: &Archive,
        stalls: &'static [usize],
    ) -> Result<String> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| storage())?;
        let address = listener.local_addr().map_err(|_| storage())?;
        let bytes = Bytes::from(fs::read(&archive.file).map_err(|_| storage())?);
        let sha256 = archive.sha256.clone();
        let attempts = Arc::new(AtomicUsize::new(0));
        let router = Router::new().fallback(move |headers: HeaderMap| {
            let bytes = bytes.clone();
            let sha256 = sha256.clone();
            let attempt = attempts.fetch_add(1, Ordering::Relaxed);
            async move {
                let start = headers
                    .get(header::RANGE)
                    .and_then(|value| value.to_str().ok()?.strip_prefix("bytes=")?.strip_suffix('-')?.parse::<usize>().ok())
                    .unwrap_or(0);
                let size = bytes.len();
                let mut response = Response::builder()
                    .header(header::CONTENT_TYPE, "application/gzip")
                    .header(header::CONTENT_LENGTH, size - start)
                    .header("x-minarbot-sha256", sha256);
                if start > 0 {
                    response = response.status(StatusCode::PARTIAL_CONTENT).header(
                        header::CONTENT_RANGE,
                        format!("bytes {start}-{}/{size}", size - 1),
                    );
                }
                let body = match stalls.get(attempt) {
                    Some(&len) => Body::from_stream(async_stream::stream! {
                        if len > 0 {
                            yield Ok::<_, std::convert::Infallible>(bytes.slice(start..start + len));
                        }
                        std::future::pending::<()>().await;
                    }),
                    None => Body::from(bytes.slice(start..)),
                };
                response.body(body).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
            }
        });
        tasks.spawn(async move { axum::serve(listener, router).await });
        Ok(format!("http://{address}"))
    }

    async fn fixture_download(
        client: &reqwest::Client,
        url: &str,
        offset: u64,
    ) -> Result<reqwest::Response> {
        let mut request = client.get(url);
        if offset > 0 {
            request = request.header(header::RANGE, format!("bytes={offset}-"));
        }
        request.send().await.map_err(PeerError::network)
    }

    async fn fixture_receive(
        root: &Path,
        stalls: &'static [usize],
    ) -> Result<(Result<u64>, Vec<u64>)> {
        let archive = fixture_archive(root).await?;
        let mut tasks = JoinSet::new();
        let url = fixture_sender(&mut tasks, &archive, stalls).await?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .read_timeout(Duration::from_millis(250))
            .build()
            .map_err(PeerError::network)?;
        let received = root.join("received");
        let mut offsets = Vec::new();
        let result = receive(
            &received,
            Download::Peer {
                id: RELEASE_ID,
                sha256: &archive.sha256,
            },
            |offset| {
                offsets.push(offset);
                fixture_download(&client, &url, offset)
            },
            || async { Ok(()) },
        )
        .await;
        Ok((result, offsets))
    }

    #[tokio::test]
    async fn peer_release_resumes_after_four_progressing_idle_timeouts() -> Result<()> {
        let root = Temporary::new(&std::env::temp_dir(), "peer-resume-test")?;
        let (result, offsets) = fixture_receive(&root.0, &[8, 8, 8, 8]).await?;
        result?;
        assert_eq!(offsets, [0, 8, 16, 24, 32]);
        assert_eq!(
            release::verify(&root.0.join("received"), RELEASE_ID)?["version"],
            "1.2.3"
        );
        Ok(())
    }

    #[tokio::test]
    async fn peer_release_times_out_after_three_consecutive_attempts_without_progress() -> Result<()>
    {
        let root = Temporary::new(&std::env::temp_dir(), "peer-resume-test")?;
        let (result, offsets) = fixture_receive(&root.0, &[8, 0, 0, 8, 0, 0, 0]).await?;
        assert_eq!(
            result.err().ok_or_else(storage)?.code,
            "peer_transfer_timeout"
        );
        assert_eq!(offsets, [0, 8, 8, 8, 16, 16, 16]);
        Ok(())
    }

    fn tar(name: &str, kind: u8, bytes: &[u8]) -> Vec<u8> {
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..108].copy_from_slice(b"0000600\0");
        header[124..136].copy_from_slice(format!("{:011o}\0", bytes.len()).as_bytes());
        header[148..156].fill(b' ');
        header[156] = kind;
        let sum: usize = header.iter().map(|b| *b as usize).sum();
        header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        let mut out = header.to_vec();
        out.extend(bytes);
        out.resize(512 + bytes.len().div_ceil(512) * 512, 0);
        out.extend([0; 1024]);
        out
    }
    fn check(bytes: &[u8]) -> Result<bool> {
        let root = std::env::temp_dir().join(format!("minarbot-tar-test-{}", uuid::Uuid::new_v4()));
        let directory = Temporary::new(&root, "test")?;
        let file = directory.0.join("archive.gz");
        let mut gzip = flate2::write::GzEncoder::new(
            File::create(&file).map_err(|_| error(500, "Fixture failed."))?,
            flate2::Compression::default(),
        );
        gzip.write_all(bytes)
            .map_err(|_| error(500, "Fixture failed."))?;
        gzip.finish().map_err(|_| error(500, "Fixture failed."))?;
        let stage = directory.0.join("stage");
        fs::create_dir(&stage).map_err(|_| error(500, "Fixture failed."))?;
        let result = extract(&file, &stage).is_ok();
        drop(directory);
        let _ = fs::remove_dir_all(root);
        Ok(result)
    }
    #[tokio::test]
    async fn stalled_http_body_reports_transfer_timeout() -> Result<()> {
        use axum::{body::Body, Router};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| storage())?;
        let address = listener.local_addr().map_err(|_| storage())?;
        let server = tokio::spawn(async move {
            let route = Router::new().fallback(|| async {
                let body = async_stream::stream! {
                    yield Ok::<_, std::convert::Infallible>("part");
                    std::future::pending::<()>().await;
                };
                (
                    [
                        ("content-type", "application/gzip"),
                        ("x-minarbot-sha256", "hash"),
                    ],
                    Body::from_stream(body),
                )
            });
            axum::serve(listener, route).await
        });
        let result = async {
            let temporary = Temporary::new(&std::env::temp_dir(), "peer-idle-test")?;
            let mut file = File::create(temporary.0.join("partial")).map_err(|_| storage())?;
            let response = reqwest::Client::builder()
                .no_proxy()
                .read_timeout(Duration::from_millis(250))
                .build()
                .map_err(PeerError::network)?
                .get(format!("http://{address}"))
                .send()
                .await
                .map_err(PeerError::network)?;
            let failure = append(&mut file, response, "hash", None)
                .await
                .err()
                .ok_or_else(storage)?;
            assert_eq!(failure.code, "peer_transfer_timeout");
            assert_eq!(file.metadata().map_err(|_| storage())?.len(), 4);
            Ok(())
        }
        .await;
        server.abort();
        result
    }

    #[test]
    fn strict_ustar_rejects_links_traversal_duplicates_and_truncation() -> Result<()> {
        assert!(check(&tar("./bin/server", b'0', b"native"))?);
        for (name, kind) in [
            ("../outside", b'0'),
            ("/outside", b'0'),
            ("dir/../outside", b'0'),
            ("symlink", b'2'),
            ("hardlink", b'1'),
            ("device", b'3'),
            ("bad\\path", b'0'),
        ] {
            assert!(!check(&tar(name, kind, b""))?, "{name}");
        }
        let mut repeated = tar("file", b'0', b"x");
        repeated.truncate(1024);
        repeated.extend(tar("file", b'0', b"y"));
        assert!(!check(&repeated)?);
        assert!(!check(&tar("file", b'0', b"x")[..514])?);
        let mut checksum = tar("file", b'0', b"x");
        checksum[0] ^= 1;
        assert!(!check(&checksum)?);
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/peer_sha_mismatch.rs"]
mod peer_sha_mismatch;
