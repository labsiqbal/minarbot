use super::{error, Result};
use crate::web::Failure;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncWriteExt, process::Command};

pub const VERSION: &str = "0.6.1";
const KEY: &str = include_str!("release-key.asc");
// Published at v0.6.1. Primary: 2B76741B414AAD6CD33E631213A005C0B9D7C3ED.
// Signing subkey: 568DC1F68F8FAECC797F3C7B006DAF6BB0EF334E.
const BASE: &str = "https://github.com/storytold/craft-anything/releases/download";

fn asset() -> Option<(&'static str, &'static str)> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    match std::env::consts::ARCH {
        "x86_64" => Some((
            "linux-x86_64",
            "83ec0730c9117f399122b315c63ed4c3a8d31ac55bddd74cb8b33cd2f76ef704",
        )),
        "aarch64" => Some((
            "linux-aarch64",
            "b4255ac806e2a598b9fc6dba4e8ed2190e49baafd2f68dd7e037f3294e472941",
        )),
        "riscv64" => Some((
            "linux-riscv64",
            "d82468c2daef97811f0b9b63dba523516860f238d46873ad6fbccccadedd62c1",
        )),
        _ => None,
    }
}

pub fn available() -> bool {
    asset().is_some()
        && ["/usr/bin/bwrap", "/usr/bin/gpgv", "/usr/bin/tar"]
            .iter()
            .all(|p| Path::new(p).is_file())
}

pub struct Temp(pub PathBuf);

impl Temp {
    pub fn new(parent: &Path) -> Result<Self> {
        let path = parent.join(format!(".craft-{}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| Failure::storage())?;
        Ok(Self(path))
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn binary(root: &Path) -> PathBuf {
    root.join(VERSION).join("craft-anything-cli")
}

pub fn installed(root: &Path) -> bool {
    fs::symlink_metadata(binary(root)).is_ok_and(|metadata| metadata.is_file())
}

async fn download(
    client: &reqwest::Client,
    name: &str,
    path: &Path,
    limit: usize,
) -> Result<String> {
    let unavailable = || error(502, "Craft Anything release download failed.");
    let mut response = client
        .get(format!("{BASE}/v{VERSION}/{name}"))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|_| unavailable())?;
    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|_| Failure::storage())?;
    let mut size = 0;
    let mut digest = Sha256::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        size += chunk.len();
        if size > limit {
            return Err(error(
                502,
                "Craft Anything release file exceeds its size limit.",
            ));
        }
        digest.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|_| Failure::storage())?;
    }
    file.sync_all().await.map_err(|_| Failure::storage())?;
    Ok(format!("{:x}", digest.finalize()))
}

async fn verify(directory: &Path) -> Result<()> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let key = KEY
        .lines()
        .skip(2)
        .take_while(|line| !line.starts_with('='))
        .collect::<String>();
    let key = STANDARD.decode(key).map_err(|_| Failure::storage())?;
    let keyring = directory.join("release.gpg");
    fs::write(&keyring, key).map_err(|_| Failure::storage())?;
    let status = Command::new("/usr/bin/gpgv")
        .env_clear()
        .kill_on_drop(true)
        .args(["--homedir"])
        .arg(directory)
        .arg("--keyring")
        .arg(keyring)
        .arg(directory.join("SHA256SUMS.txt.asc"))
        .arg(directory.join("SHA256SUMS.txt"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map_err(|_| error(503, "GPG verification is unavailable."))?;
    if !status.success() {
        return Err(error(
            502,
            "Craft Anything release signature verification failed.",
        ));
    }
    Ok(())
}

fn listed(manifest: &str, name: &str, expected: &str) -> bool {
    let mut matches = manifest.lines().filter_map(|line| {
        let (digest, path) = line.split_once("  ")?;
        (path == name).then_some(digest)
    });
    matches.next() == Some(expected) && matches.next().is_none()
}

async fn extract(
    stage: &Path,
    archive: &Path,
    prefix: &str,
    member: &str,
    output: &str,
) -> Result<()> {
    let output = stage.join(output);
    let file = File::create(&output).map_err(|_| Failure::storage())?;
    let status = Command::new("/usr/bin/tar")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .kill_on_drop(true)
        .args(["-xOzf"])
        .arg(archive)
        .arg("--")
        .arg(format!("{prefix}/{member}"))
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::null())
        .status()
        .await
        .map_err(|_| error(503, "Tar is unavailable."))?;
    if !status.success() || fs::metadata(&output).map_err(|_| Failure::storage())?.len() == 0 {
        return Err(error(502, "Craft Anything release archive is incomplete."));
    }
    Ok(())
}

pub async fn install(root: &Path) -> Result<()> {
    let (platform, expected) = asset()
        .filter(|_| available())
        .ok_or_else(|| error(503, "Craft Anything requires Linux Computer, GPG and tar."))?;
    super::super::desktop::directory(root)?;
    if installed(root) {
        return Ok(());
    }
    let temp = Temp::new(root)?;
    let prefix = format!("craft-anything-{VERSION}-{platform}");
    let name = format!("{prefix}.tar.gz");
    let client = reqwest::Client::builder()
        .no_proxy()
        .https_only(true)
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|_| Failure::storage())?;
    for file in ["SHA256SUMS.txt", "SHA256SUMS.txt.asc"] {
        download(&client, file, &temp.0.join(file), 64 * 1024).await?;
    }
    verify(&temp.0).await?;
    let manifest =
        fs::read_to_string(temp.0.join("SHA256SUMS.txt")).map_err(|_| Failure::storage())?;
    if !listed(&manifest, &name, expected) {
        return Err(error(
            502,
            "Pinned Craft Anything checksum is missing or changed.",
        ));
    }
    let archive = temp.0.join("release.tar.gz");
    if download(&client, &name, &archive, 128 * 1024 * 1024).await? != expected {
        return Err(error(
            502,
            "Craft Anything release checksum verification failed.",
        ));
    }
    let stage = temp.0.join("verified");
    fs::create_dir(&stage).map_err(|_| Failure::storage())?;
    extract(
        &stage,
        &archive,
        &prefix,
        "bin/craft-anything-cli",
        "craft-anything-cli",
    )
    .await?;
    for notice in ["LICENSE-MIT", "LICENSE-APACHE", "NOTICE"] {
        extract(
            &stage,
            &archive,
            &prefix,
            &format!("share/doc/craft-anything/{notice}"),
            notice,
        )
        .await?;
    }
    fs::set_permissions(
        stage.join("craft-anything-cli"),
        fs::Permissions::from_mode(0o500),
    )
    .map_err(|_| Failure::storage())?;
    fs::rename(stage, root.join(VERSION)).map_err(|_| Failure::storage())
}

pub fn remove(root: &Path) -> Result<()> {
    match fs::remove_dir_all(root.join(VERSION)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(Failure::storage()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn verifies_release_signature_and_rejects_tampering() -> crate::db::Result<()> {
        if !Path::new("/usr/bin/gpgv").is_file() {
            return Ok(());
        }
        let temp = Temp::new(&std::env::temp_dir()).map_err(|e| e.message)?;
        let manifest = temp.0.join("SHA256SUMS.txt");
        let signed = include_str!("signed-test-manifest.txt");
        fs::write(&manifest, signed)?;
        fs::write(
            temp.0.join("SHA256SUMS.txt.asc"),
            include_str!("signed-test-manifest.asc"),
        )?;
        verify(&temp.0).await.map_err(|e| e.message)?;
        let (platform, digest) = asset().unwrap_or((
            "linux-x86_64",
            "83ec0730c9117f399122b315c63ed4c3a8d31ac55bddd74cb8b33cd2f76ef704",
        ));
        let name = format!("craft-anything-{VERSION}-{platform}.tar.gz");
        assert!(listed(signed, &name, digest));
        assert!(!listed(
            &format!("{signed}{digest}  {name}\n"),
            &name,
            digest
        ));
        fs::write(&manifest, format!("{signed}\n"))?;
        assert!(verify(&temp.0).await.is_err());
        fs::create_dir(temp.0.join(VERSION))?;
        fs::write(binary(&temp.0), "fixture")?;
        assert!(installed(&temp.0));
        remove(&temp.0).map_err(|e| e.message)?;
        assert!(!installed(&temp.0));
        remove(&temp.0).map_err(|e| e.message)?;
        Ok(())
    }
}
