use super::*;
use crate::release::{self, Identity, Info, Ready};
use base64::{engine::general_purpose::STANDARD, Engine};
use nix::libc::{O_NOFOLLOW, O_NONBLOCK};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{ffi::OsStr, fs::OpenOptions, io::Read, os::unix::fs::OpenOptionsExt, process::Stdio};

pub const TARGETS: [&str; 4] = [
    "linux-x86_64",
    "linux-aarch64",
    "macos-x86_64",
    "macos-aarch64",
];
const CHECKSUMS: &str = "ci-checksums.json";
const SIGNED: [&str; 2] = ["latest.json", "latest.json.sig"];
const IMAGE_SIGNED: [&str; 2] = ["computer-image.json", "computer-image.json.sig"];

pub struct ImageCredentials {
    user: String,
    token: String,
}
impl ImageCredentials {
    pub fn read(ci: bool) -> Result<Self> {
        let user = env::var("MINARBOT_GHCR_USER").unwrap_or_default();
        let token = env::var("MINARBOT_GHCR_TOKEN").unwrap_or_default();
        if user.is_empty() || token.is_empty() {
            let message = if ci {
                "Set MINARBOT_GHCR_USER and MINARBOT_GHCR_TOKEN to publish the CI Computer image."
            } else {
                "Set MINARBOT_GHCR_USER and MINARBOT_GHCR_TOKEN to publish the Computer image, or pass --no-image."
            };
            return Err(message.into());
        }
        Ok(Self { user, token })
    }
}

pub fn publish_image(
    root: &Path,
    commit: &str,
    binary: &Path,
    credentials: ImageCredentials,
) -> Result<String> {
    let stage = scratch("minarbot-computer-")?;
    let context = stage.path().join("context");
    fs::create_dir(&context)?;
    for (source, name) in [
        ("computer/image/Containerfile", "Containerfile"),
        ("app/LICENSE", "LICENSE"),
        ("app/agent-delegation.LICENSE", "agent-delegation.LICENSE"),
        ("app/t3/NOTICE", "NOTICE"),
        (
            "crates/minarbot-server/whatsapp-rust.LICENSE",
            "whatsapp-rust.LICENSE",
        ),
    ] {
        copy(&root.join(source), &context.join(name))?;
    }
    copy(binary, &context.join("minarbot-server"))?;
    let command = |runtime: &str| {
        let mut command = Command::new(runtime);
        command
            .current_dir(root)
            .env("DOCKER_CONFIG", stage.path())
            .env_remove("GH_TOKEN")
            .env_remove("MINARBOT_GHCR_TOKEN");
        if runtime == "podman" {
            command.arg("--remote=false");
        }
        command
    };
    let podman =
        output(command("podman").args(["info", "--format", "{{.Host.Security.Rootless}}"]))
            .is_ok_and(|out| out.trim() == "true");
    let runtime = if podman { "podman" } else { "docker" };
    let auth_file = stage.path().join("auth.json");
    let mut login = command(runtime);
    login.arg("login");
    if podman {
        login.arg("--authfile").arg(&auth_file);
    }
    let mut login = login
        .args([
            "--username",
            &credentials.user,
            "--password-stdin",
            "ghcr.io",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let written = writeln!(
        login.stdin.take().ok_or("Missing container login stdin")?,
        "{}",
        credentials.token
    );
    if !login.wait()?.success() {
        return Err("GHCR login failed.".into());
    }
    written?;
    let tag = format!("ghcr.io/labsiqbal/minarbot-computer:{commit}");
    run(command(runtime)
        .args(["build", "--platform", "linux/amd64", "-f"])
        .arg(context.join("Containerfile"))
        .args(["-t", &tag])
        .arg(&context))?;
    let digest = if podman {
        let digest_file = stage.path().join("digest");
        run(command(runtime)
            .args(["push", "--authfile"])
            .arg(&auth_file)
            .arg("--digestfile")
            .arg(&digest_file)
            .arg(&tag))?;
        fs::read_to_string(digest_file)?.trim().to_owned()
    } else {
        let report = output(command(runtime).args(["push", &tag]))?;
        release::pattern(r"digest: (sha256:[a-f0-9]{64}) size: [0-9]+")
            .captures(&report)
            .map(|capture| capture.extract::<1>().1[0].to_owned())
            .unwrap_or_default()
    };
    if !release::image_digest(&digest) {
        return Err("GHCR push did not return a Computer image digest.".into());
    }
    Ok(digest)
}

struct Signer(Ed25519KeyPair);
impl Signer {
    fn read(path: &Path, public_key: &[u8]) -> Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(O_NOFOLLOW | O_NONBLOCK)
            .open(path)?;
        let stat = file.metadata()?;
        if !stat.is_file() || stat.permissions().mode() & 0o7777 != 0o600 {
            return Err("Signing key must be a regular file with mode 0600.".into());
        }
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        let invalid = "Could not read an Ed25519 private key from the signing key file.";
        let pem = text
            .trim()
            .strip_prefix("-----BEGIN PRIVATE KEY-----")
            .and_then(|text| text.strip_suffix("-----END PRIVATE KEY-----"))
            .ok_or(invalid)?;
        let der = STANDARD
            .decode(pem.split_whitespace().collect::<String>())
            .map_err(|_| invalid)?;
        let key = Ed25519KeyPair::from_pkcs8_maybe_unchecked(&der).map_err(|_| invalid)?;
        if key.public_key().as_ref() != public_key {
            return Err("Signing key does not match the Minarbot release public key.".into());
        }
        Ok(Self(key))
    }

    fn sign(&self, bytes: &[u8]) -> String {
        STANDARD.encode(self.0.sign(bytes).as_ref())
    }
}

fn gh(directory: &Path, args: &[&str]) -> Result<String> {
    output(Command::new("gh").current_dir(directory).args(args))
}

#[derive(Deserialize)]
struct Target {
    #[serde(rename = "type")]
    kind: String,
    sha: String,
}

#[derive(Deserialize)]
struct Reference {
    #[serde(rename = "ref")]
    name: String,
    object: Target,
}

fn tag_target(root: &Path, tag: &str) -> Result<Option<Target>> {
    let endpoint = format!("repos/{REPOSITORY}/git");
    let refs: Vec<Reference> = serde_json::from_str(&gh(
        root,
        &["api", &format!("{endpoint}/matching-refs/tags/{tag}")],
    )?)?;
    let Some(reference) = refs
        .into_iter()
        .find(|reference| reference.name == format!("refs/tags/{tag}"))
    else {
        return Ok(None);
    };
    let mut target = reference.object;
    while target.kind == "tag" {
        target = serde_json::from_str(&gh(
            root,
            &[
                "api",
                &format!("{endpoint}/tags/{}", target.sha),
                "--jq",
                ".object",
            ],
        )?)?;
    }
    Ok(Some(target))
}

#[derive(Deserialize)]
struct RemoteAsset {
    name: String,
    state: String,
    digest: Option<String>,
}
impl RemoteAsset {
    fn matches(&self, hash: &str) -> bool {
        self.state == "uploaded" && self.digest.as_deref() == Some(&format!("sha256:{hash}"))
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Release {
    #[serde(default)]
    assets: Vec<RemoteAsset>,
    body: String,
    is_draft: bool,
    name: Option<String>,
    target_commitish: String,
}

fn remote(root: &Path, tag: &str, ci: bool) -> Result<Option<Release>> {
    let fields = if ci {
        "assets,body,isDraft,targetCommitish"
    } else {
        "assets,body,isDraft,name,targetCommitish"
    };
    let result = Command::new("gh")
        .current_dir(root)
        .args(["release", "view", tag, "-R", REPOSITORY, "--json", fields])
        .env_remove("MINARBOT_GHCR_TOKEN")
        .output()?;
    if result.status.success() {
        return Ok(Some(serde_json::from_slice(&result.stdout)?));
    }
    let error = String::from_utf8_lossy(&result.stderr);
    if result.status.code() == Some(1) && error.trim() == "release not found" {
        return Ok(None);
    }
    Err(format!(
        "gh release view failed: {} ({})",
        error.trim(),
        result.status
    )
    .into())
}

struct Asset {
    path: PathBuf,
    name: String,
    hash: String,
}
impl Asset {
    fn read(path: PathBuf) -> Result<Self> {
        Ok(Self {
            name: path
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or("Invalid asset name")?
                .to_owned(),
            hash: sha256(&path)?,
            path,
        })
    }
}

fn check_assets<'a>(
    tag: &str,
    release: &Release,
    assets: &'a [Asset],
    ci: bool,
) -> Result<Vec<(&'a Asset, bool)>> {
    if release
        .assets
        .iter()
        .any(|remote| !assets.iter().any(|asset| asset.name == remote.name))
    {
        return Err(format!(
            "{} {tag} has unexpected assets.",
            if ci { "Draft" } else { "GitHub release" }
        )
        .into());
    }
    let mut pending = Vec::new();
    for asset in assets {
        let remote = release
            .assets
            .iter()
            .find(|remote| remote.name == asset.name);
        if remote.is_some_and(|remote| remote.matches(&asset.hash)) {
            continue;
        }
        if !release.is_draft || remote.is_some_and(|remote| remote.state != "starter") {
            return Err(if ci {
                format!("Draft {tag} has a different {}.", asset.name)
            } else {
                format!(
                    "GitHub release {tag} has a missing or different asset: {}.",
                    asset.name
                )
            }
            .into());
        }
        pending.push((asset, remote.is_some()));
    }
    Ok(pending)
}

fn upload_pending(
    root: &Path,
    tag: &str,
    directory: &Path,
    pending: Vec<(&Asset, bool)>,
) -> Result<()> {
    for (asset, starter) in &pending {
        if *starter {
            gh(
                root,
                &[
                    "release",
                    "delete-asset",
                    tag,
                    "-R",
                    REPOSITORY,
                    &asset.name,
                    "--yes",
                ],
            )?;
        }
    }
    if !pending.is_empty() {
        let mut command = Command::new("gh");
        command
            .current_dir(directory)
            .args(["release", "upload", tag, "-R", REPOSITORY]);
        for (asset, _) in pending {
            command.arg(asset.path.strip_prefix(directory)?);
        }
        output(&mut command)?;
    }
    Ok(())
}

fn runtime_manifest(info: &Info, archive: &Asset) -> Result<Value> {
    Ok(json!({
        "version": info.version, "id": info.id, "commit": info.commit, "archive": archive.name,
        "sha256": archive.hash, "size": fs::metadata(&archive.path)?.len(), "notes": info.notes, "created": info.created,
    }))
}

fn signed_pair(
    directory: &Path,
    names: [&str; 2],
    value: &Value,
    signer: &Signer,
    cached: bool,
) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    let signature = signer.sign(&bytes);
    let manifest = directory.join(names[0]);
    let signature_path = directory.join(names[1]);
    if cached {
        if fs::read(manifest)? != bytes || fs::read(signature_path)? != signature.as_bytes() {
            return Err("Cached signed release assets changed.".into());
        }
    } else {
        files::durable(&manifest, &bytes)?;
        files::durable(&signature_path, signature.as_bytes())?;
    }
    Ok(())
}

fn signed_assets(
    releases: &Path,
    directory: &Path,
    info: &Info,
    signer: &Signer,
    desktop: &[Asset],
) -> Result<Vec<Asset>> {
    let bundle = directory.join("signed");
    let cached = bundle.exists();
    let stage = if cached {
        None
    } else {
        Some(private_stage(".signing-", directory)?)
    };
    let path = stage.as_ref().map_or(bundle.as_path(), |dir| dir.path());
    let archive_name = format!("minarbot-{}-linux-x86_64.tar.gz", info.version);
    let archive_path = path.join(&archive_name);
    if !cached {
        archive(&releases.join(&info.id), &archive_path)?;
    }
    let archive = Asset::read(archive_path)?;
    let mut value = runtime_manifest(info, &archive)?;
    if !desktop.is_empty() {
        let mut summary = json!({});
        for asset in std::iter::once(&archive).chain(desktop) {
            summary[&asset.name] =
                json!({"size": fs::metadata(&asset.path)?.len(), "sha256": asset.hash});
        }
        value["assets"] = summary;
    }
    signed_pair(path, SIGNED, &value, signer, cached)?;
    let mut assets = vec![archive_name, SIGNED[0].into(), SIGNED[1].into()];
    if let Some(image) = &info.computer_image {
        // Legacy updater rejects unknown fields in latest.json.
        value["computer_image"] = json!(image);
        signed_pair(path, IMAGE_SIGNED, &value, signer, cached)?;
        assets.extend(IMAGE_SIGNED.map(str::to_owned));
    }
    if let Some(stage) = stage {
        fs::rename(stage.path(), &bundle)?;
        files::sync_dir(directory)?;
    }
    assets
        .into_iter()
        .map(|name| Asset::read(bundle.join(name)))
        .collect()
}

fn publish(
    root: &Path,
    releases: &Path,
    id: &str,
    key_path: &Path,
    public_key: &[u8],
) -> Result<()> {
    let info = release::verify(releases, id)?;
    let directory = releases.join(".github").join(id);
    if directory.join("dry-run").exists() {
        return Err("Dry-run assets cannot be uploaded.".into());
    }
    let ready_path = directory.join("ready.json");
    if !ready_path.exists() {
        return Err(
            "This local release has no completed public release build with explicit --notes."
                .into(),
        );
    }
    let ready: Ready = serde_json::from_slice(&fs::read(ready_path)?)?;
    if ready.release != sha256(&releases.join(id).join("release.json"))? {
        return Err("Local release metadata changed after the release build.".into());
    }
    let mut desktop = Vec::new();
    if !ready.tauri.is_empty() {
        let platform = format!("linux-{}", env::consts::ARCH);
        let names = desktop::tauri_names(&info.version, &platform);
        if ready.tauri.len() != names.len() {
            return Err("Missing staged Tauri release assets.".into());
        }
        desktop::verify_feed(&directory, &info.version, &platform)?;
        for name in names {
            let asset = Asset::read(directory.join(&name))?;
            if ready.tauri.get(&name) != Some(&asset.hash) {
                return Err("Local Tauri assets changed after the release build.".into());
            }
            desktop.push(asset);
        }
    }
    let signer = Signer::read(key_path, public_key)?;
    let mut assets = signed_assets(releases, &directory, &info, &signer, &desktop)?;
    assets.extend(desktop);
    let tag = format!("v{}", info.version);
    let title = format!("Minarbot {}", info.version);
    let existing = remote(root, &tag, false)?;
    let target = tag_target(root, &tag)?;
    if target
        .as_ref()
        .is_some_and(|target| target.kind != "commit" || target.sha != info.commit)
        || (target.is_none() && existing.as_ref().is_some_and(|release| !release.is_draft))
    {
        return Err(format!(
            "GitHub tag {tag} does not point to release commit {}.",
            info.commit
        )
        .into());
    }
    if let Some(existing) = existing {
        if existing.name.as_deref() != Some(&title)
            || existing.body != info.notes
            || (target.is_none() && existing.target_commitish != info.commit)
        {
            return Err(format!("GitHub release {tag} has different release metadata.").into());
        }
        let pending = check_assets(&tag, &existing, &assets, false)?;
        if existing.is_draft {
            check_clean(root)?;
            upload_pending(root, &tag, &directory, pending)?;
            check_clean(root)?;
            gh(
                root,
                &["release", "edit", &tag, "-R", REPOSITORY, "--draft=false"],
            )?;
        }
    } else {
        check_clean(root)?;
        // gh creates a draft and publishes after uploading these assets.
        let mut command = Command::new("gh");
        command.current_dir(&directory).args([
            "release",
            "create",
            &tag,
            "-R",
            REPOSITORY,
            "--target",
            &info.commit,
            "--title",
            &title,
            "--notes",
            &info.notes,
        ]);
        for asset in &assets {
            command.arg(asset.path.strip_prefix(&directory)?);
        }
        output(&mut command)?;
    }
    println!("Published {tag} to {REPOSITORY}");
    Ok(())
}

pub fn upload(
    root: &Path,
    releases: &Path,
    id: &str,
    key_path: &Path,
    public_key: &[u8],
) -> Result<()> {
    publish(root, releases, id, key_path, public_key).map_err(|error|
        format!("GitHub publication failed. Local release {id} is unchanged. {error}\nAfter resolving the error, retry with --github-only {id} and the same --releases directory.").into())
}

pub fn preview(releases: &Path, directory: &Path, info: &Info) -> Result<()> {
    files::mkdir(directory)?;
    let path = directory.join(format!("minarbot-{}-linux-x86_64.tar.gz", info.version));
    archive(&releases.join(&info.id), &path)?;
    let asset = Asset::read(path)?;
    let value = runtime_manifest(info, &asset)?;
    files::durable(&directory.join(SIGNED[0]), &serde_json::to_vec(&value)?)?;
    let mut names = vec![asset.name, SIGNED[0].into(), SIGNED[1].into()];
    let tauri = desktop::tauri_names(&info.version, &format!("linux-{}", env::consts::ARCH));
    if tauri.iter().all(|name| directory.join(name).is_file()) {
        names.extend(tauri);
    }
    println!("Dry run staged unsigned assets in {}", directory.display());
    println!("Planned GitHub assets: {}", names.join(", "));
    Ok(())
}

fn ci_names(version: &str) -> Vec<String> {
    TARGETS
        .map(|target| format!("minarbot-{version}-{target}.tar.gz"))
        .into_iter()
        .chain(
            desktop::PLATFORMS
                .into_iter()
                .flat_map(|platform| desktop::tauri_names(version, platform)),
        )
        .collect()
}

fn verify_ci_tauri(directory: &Path, version: &str) -> Result<()> {
    for platform in desktop::PLATFORMS {
        desktop::verify_feed(directory, version, platform)?;
    }
    Ok(())
}

fn ci_assets(directory: &Path, names: &[String]) -> Result<(Vec<Asset>, Value)> {
    let mut assets = Vec::new();
    let mut summary = json!({});
    for name in names {
        let path = directory.join(name);
        let size = fs::metadata(&path)?.len();
        if !(1..=(1 << 30)).contains(&size) {
            return Err(format!("Invalid CI asset size: {name}").into());
        }
        let asset = Asset::read(path)?;
        summary[name] = json!({"size": size, "sha256": asset.hash});
        assets.push(asset);
    }
    Ok((assets, summary))
}

#[derive(Serialize, Deserialize)]
struct Checksums {
    #[serde(flatten)]
    identity: Identity,
    assets: Value,
    #[serde(rename = "computerImage", skip_serializing_if = "Option::is_none")]
    computer_image: Option<String>,
}

fn check_draft(
    root: &Path,
    tag: &str,
    release: Option<Release>,
    info: &Identity,
) -> Result<Release> {
    let release = release
        .filter(|release| {
            release.is_draft
                && release.target_commitish == info.commit
                && release.body == info.notes
        })
        .ok_or_else(|| {
            format!(
                "Expected a draft {tag} for commit {} with matching notes.",
                info.commit
            )
        })?;
    if tag_target(root, tag)?
        .is_some_and(|target| target.kind != "commit" || target.sha != info.commit)
    {
        return Err(format!("GitHub tag {tag} points at a different commit.").into());
    }
    Ok(release)
}

pub fn draft_ci(root: &Path, directory: &Path, notes: String) -> Result<()> {
    let info = release::ci_identity(
        root,
        release::version(&root.join("app"))?,
        git(root, &["rev-parse", "HEAD"])?,
        notes,
    )?;
    let tag = format!("v{}", info.version);
    let names = ci_names(&info.version);
    verify_ci_tauri(directory, &info.version)?;
    let entries = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
    for entry in entries {
        let name = entry.file_name();
        if name != CHECKSUMS
            && !names
                .iter()
                .any(|allowed| *allowed == name.to_string_lossy())
        {
            return Err("Unexpected CI build output.".into());
        }
    }
    let (mut assets, summary) = ci_assets(directory, &names)?;
    let credentials = ImageCredentials::read(true)?;
    let stage = scratch("minarbot-ci-image-")?;
    run(Command::new("tar")
        .arg("-xzf")
        .arg(directory.join(&names[0]))
        .arg("-C")
        .arg(stage.path())
        .arg("./bin/minarbot-server"))?;
    let image = publish_image(
        root,
        &info.commit,
        &stage.path().join("bin/minarbot-server"),
        credentials,
    )?;
    let checksums = Checksums {
        identity: info.clone(),
        assets: summary,
        computer_image: Some(image),
    };
    files::durable(&directory.join(CHECKSUMS), &serde_json::to_vec(&checksums)?)?;
    assets.push(Asset::read(directory.join(CHECKSUMS))?);
    if remote(root, &tag, true)?.is_none() {
        gh(
            root,
            &[
                "release",
                "create",
                &tag,
                "-R",
                REPOSITORY,
                "--draft",
                "--target",
                &info.commit,
                "--title",
                &format!("Minarbot {}", info.version),
                "--notes",
                &info.notes,
            ],
        )?;
    }
    let draft = check_draft(root, &tag, remote(root, &tag, true)?, &info)?;
    upload_pending(
        root,
        &tag,
        directory,
        check_assets(&tag, &draft, &assets, true)?,
    )?;
    println!(
        "Draft {tag} is ready. Sign and publish with --sign-ci {}.",
        info.version
    );
    Ok(())
}

pub fn sign_ci(root: &Path, version: &str, key: &Path, public_key: &[u8]) -> Result<()> {
    if !release::valid_version(version) {
        return Err("Pass --sign-ci the release version, without v.".into());
    }
    let tag = format!("v{version}");
    let draft = remote(root, &tag, true)?
        .filter(|release| release.is_draft)
        .ok_or_else(|| format!("Expected a draft release for {tag}."))?;
    let info = release::ci_identity(
        root,
        version.into(),
        draft.target_commitish.clone(),
        draft.body.clone(),
    )?;
    git(root, &["merge-base", "--is-ancestor", &info.commit, "HEAD"])?;
    let package: Value = serde_json::from_str(&git(
        root,
        &["show", &format!("{}:app/package.json", info.commit)],
    )?)?;
    if package["version"] != version {
        return Err("Draft version does not match the release commit.".into());
    }
    let draft = check_draft(root, &tag, Some(draft), &info)?;
    let names = ci_names(version);
    let required: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .chain([CHECKSUMS])
        .collect();
    if draft.assets.iter().any(|asset| {
        !required.contains(&asset.name.as_str())
            && !SIGNED.contains(&asset.name.as_str())
            && !IMAGE_SIGNED.contains(&asset.name.as_str())
    }) || required.iter().any(|name| {
        !draft
            .assets
            .iter()
            .any(|asset| asset.name == *name && asset.state == "uploaded")
    }) {
        return Err("Draft is missing CI assets or has unexpected assets.".into());
    }
    let stage = scratch("minarbot-ci-sign-")?;
    let directory = stage.path();
    let mut command = Command::new("gh");
    command
        .current_dir(root)
        .args(["release", "download", &tag, "-R", REPOSITORY, "--dir"])
        .arg(directory);
    for name in required {
        command.args(["--pattern", name]);
    }
    output(&mut command)?;
    verify_ci_tauri(directory, version)?;
    let (mut assets, summary) = ci_assets(directory, &names)?;
    assets.push(Asset::read(directory.join(CHECKSUMS))?);
    for asset in &assets {
        if !draft
            .assets
            .iter()
            .any(|remote| remote.name == asset.name && remote.matches(&asset.hash))
        {
            return Err(format!("GitHub asset digest mismatch: {}", asset.name).into());
        }
    }
    let bytes = fs::read(directory.join(CHECKSUMS))?;
    let checksums: Checksums = serde_json::from_slice(&bytes)?;
    if checksums
        .computer_image
        .as_deref()
        .is_some_and(|image| !release::image_digest(image))
    {
        return Err("Invalid CI Computer image digest.".into());
    }
    let expected = Checksums {
        identity: info.clone(),
        assets: summary.clone(),
        computer_image: checksums.computer_image.clone(),
    };
    if bytes != serde_json::to_vec(&expected)? {
        return Err("CI checksums do not match the downloaded assets.".into());
    }
    let mut manifest = serde_json::to_value(&info)?;
    manifest["archive"] = json!(names[0]);
    manifest["size"] = summary[&names[0]]["size"].clone();
    manifest["sha256"] = summary[&names[0]]["sha256"].clone();
    manifest["assets"] = summary;
    let signer = Signer::read(key, public_key)?;
    signed_pair(directory, SIGNED, &manifest, &signer, false)?;
    for name in SIGNED {
        assets.push(Asset::read(directory.join(name))?);
    }
    if let Some(image) = checksums.computer_image {
        manifest["computer_image"] = json!(image);
        signed_pair(directory, IMAGE_SIGNED, &manifest, &signer, false)?;
        for name in IMAGE_SIGNED {
            assets.push(Asset::read(directory.join(name))?);
        }
    }
    check_clean(root)?;
    let latest = check_draft(root, &tag, remote(root, &tag, true)?, &info)?;
    upload_pending(
        root,
        &tag,
        directory,
        check_assets(&tag, &latest, &assets, true)?,
    )?;
    let ready = check_draft(root, &tag, remote(root, &tag, true)?, &info)?;
    if !check_assets(&tag, &ready, &assets, true)?.is_empty() {
        return Err("Draft changed during signing.".into());
    }
    gh(
        root,
        &[
            "release",
            "edit",
            &tag,
            "-R",
            REPOSITORY,
            "--draft=false",
            "--latest",
        ],
    )?;
    println!("Signed and published {tag} to {REPOSITORY}");
    Ok(())
}
