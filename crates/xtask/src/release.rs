use super::*;
use crate::leptos;
use chrono::{DateTime, SecondsFormat, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeSet, sync::LazyLock};

static VERSION: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"^[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?$"));
static CLAIM: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"(?m)^Claimed by [^\r\n]+, branch (t3/\S+)[ \t]*\r?$"));

pub fn pattern(regex: &str) -> Regex {
    match Regex::new(regex) {
        Ok(regex) => regex,
        Err(error) => panic!("Invalid built-in regex: {error}"),
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Info {
    pub id: String,
    pub version: String,
    pub commit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issues: Option<Vec<u64>>,
    pub created: String,
    pub node: String,
    pub notes: String,
    pub files: BTreeMap<String, String>,
    #[serde(rename = "computerImage", skip_serializing_if = "Option::is_none")]
    pub computer_image: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Identity {
    pub version: String,
    pub id: String,
    pub commit: String,
    pub notes: String,
    pub created: String,
}

#[derive(Serialize, Deserialize)]
pub struct Ready {
    pub release: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desktop: Option<[String; 2]>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tauri: BTreeMap<String, String>,
}

pub fn valid_version(version: &str) -> bool {
    VERSION.is_match(version)
}

pub fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn image_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|digest| hex(digest, 64))
}

pub fn version(source: &Path) -> Result<String> {
    Ok(files::json(&source.join("package.json"))?["version"]
        .as_str()
        .ok_or("Missing package version")?
        .to_owned())
}

fn validate(info: &Info) -> Result<()> {
    let hash = info.id.strip_prefix(&format!("{}-", info.version));
    if !files::release_id(&info.id)
        || !valid_version(&info.version)
        || !hex(&info.commit, 40)
        || !hash.is_some_and(|hash| info.commit.starts_with(hash))
        || DateTime::parse_from_rfc3339(&info.created).is_err()
        || info.notes.encode_utf16().count() > 16000
        || info
            .computer_image
            .as_deref()
            .is_some_and(|image| !image_digest(image))
    {
        return Err("Invalid release metadata.".into());
    }
    for (path, hash) in &info.files {
        if path.split('/').any(|part| ["", ".", ".."].contains(&part))
            || path
                .bytes()
                .any(|byte| byte == b'\\' || byte.is_ascii_control())
            || !hex(hash, 64)
        {
            return Err("Invalid release file manifest.".into());
        }
    }
    if let Some(issues) = &info.issues {
        if issues
            .iter()
            .any(|issue| !(1..=9_007_199_254_740_991).contains(issue))
            || issues.iter().collect::<BTreeSet<_>>().len() != issues.len()
        {
            return Err("Invalid release issues.".into());
        }
    }
    Ok(())
}

pub fn verify(releases: &Path, id: &str) -> Result<Info> {
    let info = serde_json::from_value(files::verify(releases, id)?)?;
    validate(&info)?;
    Ok(info)
}

pub fn stage_runtime(source: &Path, stage: &Path) -> Result<()> {
    for name in [
        "bin",
        "dist",
        "package.json",
        "LICENSE",
        "novnc.LICENSE",
        "t3/NOTICE",
        "t3/provenance.json",
        "scripts/minarbot-command.sh",
        "shared-data-worker.py",
    ] {
        let from = source.join(name);
        if from.symlink_metadata().is_ok() {
            let to = if name == "shared-data-worker.py" {
                stage.join("app").join(name)
            } else {
                stage.join(name)
            };
            copy(&from, &to)?;
        }
    }
    Ok(())
}

pub fn native_dependencies(binary: &Path) -> Result<()> {
    let deps = output(Command::new("ldd").arg(binary))?;
    if deps.to_ascii_lowercase().contains("not found") {
        return Err(format!("Native dependency missing for {}: {deps}", binary.display()).into());
    }
    for line in deps.lines() {
        let path = line
            .split_once("=>")
            .map_or(line, |(_, right)| right)
            .split_whitespace()
            .next()
            .unwrap_or_default();
        if path.starts_with('/') {
            let resolved = fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
            if !["/lib", "/usr/lib", "/lib64"]
                .iter()
                .any(|root| resolved.starts_with(root))
            {
                return Err(format!(
                    "Native dependency outside system loader paths for {}: {path}",
                    binary.display()
                )
                .into());
            }
        }
    }
    Ok(())
}

fn native_runtime(root: &Path, binaries: &Path, stage: &Path) -> Result<()> {
    let bin = stage.join("bin");
    files::mkdir(&bin)?;
    for name in ["minarbot-server", "minarbot-launcher"] {
        let destination = bin.join(name);
        copy(&binaries.join(name), &destination)?;
        if cfg!(target_os = "linux") {
            native_dependencies(&destination)?;
        }
    }
    for (from, to) in [
        (
            "crates/minarbot-launcher/provenance.json",
            "launcher-provenance.json",
        ),
        ("crates/minarbot-launcher/NOTICE", "launcher.NOTICE"),
        ("crates/minarbot-server/provenance.json", "provenance.json"),
        ("app/LICENSE", "LICENSE"),
        ("app/agent-delegation.LICENSE", "agent-delegation.LICENSE"),
    ] {
        copy(&root.join(from), &bin.join(to))?;
    }
    let whatsapp = root.join("crates/minarbot-server/whatsapp-rust.LICENSE");
    if whatsapp.exists() {
        copy(&whatsapp, &bin.join("whatsapp-rust.LICENSE"))?;
    }
    Ok(())
}

fn parse_issues(value: Option<&String>) -> Result<Vec<u64>> {
    let mut issues = Vec::new();
    if let Some(value) = value {
        for part in value.split(',') {
            let issue = part
                .parse::<u64>()
                .ok()
                .filter(|n| {
                    (1..=9_007_199_254_740_991).contains(n)
                        && part.bytes().all(|b| b.is_ascii_digit())
                })
                .ok_or("Invalid issues. Use --issues 30,31 with positive issue numbers.")?;
            if !issues.contains(&issue) {
                issues.push(issue);
            }
        }
    }
    Ok(issues)
}

fn ancestry(root: &Path, issues: &[u64], commit: &str) -> Result<()> {
    if issues.is_empty() {
        eprintln!("WARNING: RELEASE ANCESTRY GATE SKIPPED. Supply --issues 30,31 for every shipped issue.");
    }
    for issue in issues {
        let report = output(Command::new("gh").args(["issue", "view", &issue.to_string(), "-R", REPOSITORY, "--json", "comments"]).current_dir(root))
            .map_err(|_| format!("Issue #{issue}: cannot read claim comments. Check gh authentication and access to {REPOSITORY}, then retry."))?;
        let value: Value = serde_json::from_str(&report)?;
        let invalid =
            || format!("Issue #{issue}: invalid comments. Fix the issue reader and retry.");
        let mut branches = Vec::new();
        for comment in value["comments"].as_array().ok_or_else(invalid)? {
            let body = comment["body"].as_str().ok_or_else(invalid)?;
            for branch in CLAIM
                .captures_iter(body)
                .map(|capture| capture.extract::<1>().1[0])
            {
                if !branches.contains(&branch) {
                    branches.push(branch);
                }
            }
        }
        if branches.is_empty() {
            return Err(format!("Issue #{issue}: no claim comment. Add \"Claimed by <agent>, branch t3/<slug>\" and retry.").into());
        }
        for branch in branches {
            let reference = format!("refs/heads/{branch}");
            if git(root, &["show-ref", "--verify", "--quiet", &reference]).is_err() {
                return Err(format!("Issue #{issue}: local branch {branch} is missing. Restore the task branch locally and retry.").into());
            }
            if git(root, &["merge-base", "--is-ancestor", &reference, commit]).is_err() {
                return Err(format!("Issue #{issue}: {branch} is not an ancestor of {commit}. Merge the branch tip and retry.").into());
            }
        }
    }
    Ok(())
}

fn run_build(root: &Path, source: &Path) -> Result<()> {
    for script in ["lint", "build"] {
        run(Command::new("npm")
            .current_dir(source)
            .args(["run", script]))?;
    }
    if root.join("crates/web/Cargo.toml").is_file() {
        leptos::build(root)?;
    }
    run(Command::new("npm")
        .current_dir(source)
        .args(["run", "test"]))?;
    if root.join("Cargo.toml").is_file() {
        let checks: &[&[&str]] = &[
            &["fmt", "--check"],
            &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
            &["test", "--workspace"],
            &["build", "--workspace", "--release", "--locked"],
        ];
        for args in checks {
            run(Command::new("cargo")
                .current_dir(root)
                .env("CARGO_TARGET_DIR", root.join("target"))
                .args(*args))?;
        }
        #[cfg(test)]
        if root.join("scripts/resource-gate-fixture").is_file() {
            for name in ["resource-gate-fixture", "production-env-gate-fixture"] {
                run(Command::new(root.join("scripts").join(name)).current_dir(root))?;
            }
            return Ok(());
        }
        let target = root.join("target/release");
        #[cfg(not(test))]
        let executable = env::current_exe()?;
        #[cfg(test)]
        let executable = env::var_os("MINARBOT_XTASK_BINARY")
            .map_or_else(|| root.join("target/debug/xtask"), PathBuf::from);
        let mut resource = Command::new(&executable);
        resource
            .current_dir(root)
            .env("TMPDIR", "/dev/shm")
            .arg("resource-gate")
            .arg(target.join("minarbot-server"))
            .arg(root.join("scripts/budgets.json"))
            .arg(target.join("minarbot-launcher"));
        run(&mut resource)?;
        run(Command::new("systemd-run")
            .current_dir(root)
            .args([
                "--user",
                "--scope",
                "-q",
                "-p",
                "MemoryMax=4G",
                "-p",
                "MemorySwapMax=0",
            ])
            .arg(executable)
            .arg("production-env-gate")
            .arg(target.join("minarbot-server"))
            .arg(target.join("minarbot-launcher")))?;
    }
    Ok(())
}

pub fn desktop_assets(directory: &Path, version: &str) -> Result<[PathBuf; 2]> {
    let name = format!("Minarbot-{version}-x86_64.AppImage");
    let image = directory.join(&name);
    let metadata = directory.join("latest-linux.yml");
    let yaml = fs::read_to_string(&metadata)?;
    if !image.is_file()
        || !yaml.lines().any(|line| line == format!("path: {name}"))
        || !yaml
            .lines()
            .any(|line| line == format!("version: {version}"))
    {
        return Err(
            "Desktop build did not produce the matching AppImage and latest-linux.yml.".into(),
        );
    }
    Ok([image, metadata])
}

fn build_desktop(
    root: &Path,
    runtime: &Path,
    directory: &Path,
    version: &str,
    dry_run: bool,
) -> Result<([String; 2], BTreeMap<String, String>)> {
    files::mkdir(directory)?;
    let stage = private_stage(".desktop-", directory)?;
    let archive_path = stage.path().join("runtime.tar.gz");
    archive(runtime, &archive_path)?;
    let tauri_names = desktop::release(root, &archive_path, stage.path(), version, dry_run)?;
    let [image, yaml] = desktop_assets(stage.path(), version)?;
    let hashes = [sha256(&image)?, sha256(&yaml)?];
    let mut tauri = BTreeMap::new();
    for name in tauri_names {
        let file = stage.path().join(&name);
        tauri.insert(name.clone(), sha256(&file)?);
        fs::rename(file, directory.join(name))?;
    }
    for file in [image, yaml] {
        fs::rename(
            &file,
            directory.join(file.file_name().ok_or("Missing desktop asset name")?),
        )?;
    }
    files::sync_dir(directory)?;
    Ok((hashes, tauri))
}

fn metadata(path: &Path, info: &Info, pretty: bool) -> Result<()> {
    let mut bytes = if pretty {
        serde_json::to_vec_pretty(info)?
    } else {
        serde_json::to_vec(info)?
    };
    bytes.push(b'\n');
    durable(path, &bytes, 0o644)
}

pub fn ci_identity(
    root: &Path,
    version: String,
    commit: String,
    notes: String,
) -> Result<Identity> {
    if !valid_version(&version)
        || !hex(&commit, 40)
        || notes.trim().is_empty()
        || notes.encode_utf16().count() > 16000
    {
        return Err("Invalid CI release version, commit or notes.".into());
    }
    let time = git(root, &["show", "-s", "--format=%cI", &commit])?;
    let created = DateTime::parse_from_rfc3339(&time)?
        .with_timezone(&Utc)
        .to_rfc3339_opts(SecondsFormat::Millis, true);
    Ok(Identity {
        id: format!("{version}-{}", &commit[..8]),
        version,
        commit,
        notes,
        created,
    })
}

pub fn pack_ci(root: &Path, binaries: &Path, directory: &Path, notes: String) -> Result<()> {
    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        ("macos", "x86_64") => "macos-x86_64",
        ("macos", "aarch64") => "macos-aarch64",
        _ => return Err("Unsupported native CI packaging target.".into()),
    }
    .to_owned();
    if !github::TARGETS.contains(&target.as_str()) {
        return Err("Unsupported native CI packaging target.".into());
    }
    let source = root.join("app");
    let identity = ci_identity(
        root,
        version(&source)?,
        git(root, &["rev-parse", "HEAD"])?,
        notes,
    )?;
    if !source.join("dist/index.html").is_file() {
        return Err("Build app before CI packaging.".into());
    }
    leptos::require_build(&source)?;
    let stage = scratch("minarbot-package-")?;
    stage_runtime(&source, stage.path())?;
    native_runtime(root, binaries, stage.path())?;
    machine(root, stage.path(), true)?;
    // CI metadata historically serializes identity before runtime metadata.
    let mut value = serde_json::to_value(identity)?;
    value["node"] = json!("rust");
    value["files"] = serde_json::to_value(files::manifest(stage.path())?)?;
    validate(&serde_json::from_value(value.clone())?)?;
    let mut bytes = serde_json::to_vec(&value)?;
    bytes.push(b'\n');
    durable(&stage.path().join("release.json"), &bytes, 0o644)?;
    fs::create_dir_all(directory)?;
    let name = format!(
        "minarbot-{}-{target}.tar.gz",
        value["version"].as_str().ok_or("Missing version")?
    );
    let temporary = private_stage(".package-", directory)?;
    archive(stage.path(), &temporary.path().join(&name))?;
    fs::rename(temporary.path().join(&name), directory.join(&name))?;
    println!("{}", directory.join(name).display());
    Ok(())
}

fn machine(root: &Path, stage: &Path, ci: bool) -> Result<()> {
    let directory = stage.join("machine");
    if ci {
        fs::create_dir(&directory)?;
    } else {
        files::mkdir(&directory)?;
    }
    for name in ["install-machine.sh", "uninstall-machine.sh"] {
        copy(&root.join("scripts").join(name), &directory.join(name))?;
    }
    if ci {
        copy(
            &root.join("scripts/install.sh"),
            &directory.join("install.sh"),
        )?;
    }
    Ok(())
}

struct Local<'a> {
    root: &'a Path,
    scripts_root: &'a Path,
    source: &'a Path,
    releases: &'a Path,
    allow_dirty: bool,
    notes: Option<&'a str>,
    issues: Vec<u64>,
    build: bool,
}

impl Local<'_> {
    fn build(&self, image: Option<github::ImageCredentials>) -> Result<Info> {
        let Self {
            root,
            source,
            releases,
            notes,
            issues,
            ..
        } = self;
        if !self.allow_dirty {
            check_clean(root)?;
        }
        let version = version(source)?;
        let commit = git(root, &["rev-parse", "HEAD"])?;
        if !valid_version(&version) || !hex(&commit, 40) {
            return Err("Invalid release metadata.".into());
        }
        ancestry(root, issues, &commit)?;
        let id = format!("{version}-{}", &commit[..8]);
        let target = releases.join(&id);
        if target.symlink_metadata().is_ok() {
            return Err(format!("Release already exists: {id}").into());
        }
        let binaries = root.join("target/release");
        if self.build {
            run_build(root, source)?;
        }
        let computer_image = if let Some(credentials) = image {
            check_head(root, &commit)?;
            Some(github::publish_image(
                root,
                &commit,
                &binaries.join("minarbot-server"),
                credentials,
            )?)
        } else {
            None
        };
        files::mkdir(releases)?;
        let stage = private_stage(".staging-", releases)?;
        stage_runtime(source, stage.path())?;
        if root.join("Cargo.toml").is_file()
            && binaries.join("minarbot-server").is_file()
            && binaries.join("minarbot-launcher").is_file()
        {
            native_runtime(root, &binaries, stage.path())?;
        }
        // Production source and git root are the same. Test fixtures may supply their own source.
        machine(self.scripts_root, stage.path(), false)?;
        let info = Info {
            id,
            version,
            commit,
            issues: Some(issues.clone()),
            created: DateTime::<Utc>::from(std::time::SystemTime::now())
                .to_rfc3339_opts(SecondsFormat::Millis, true),
            node: "rust".into(),
            notes: match notes {
                Some(notes) => (*notes).to_owned(),
                None => git(root, &["log", "-5", "--pretty=%s"])?,
            },
            files: files::manifest(stage.path())?,
            computer_image,
        };
        validate(&info)?;
        metadata(&stage.path().join("release.json"), &info, true)?;
        fs::rename(stage.path(), &target)?;
        files::sync_dir(releases)?;
        files::mkdir(&releases.join("feed"))?;
        metadata(&releases.join("feed/latest.json"), &info, true)?;
        Ok(info)
    }
}

const SWITCHES: &[&str] = &[
    "--allow-dirty",
    "--no-github",
    "--desktop",
    "--no-image",
    "--dry-run",
];
const VALUES: &[&str] = &[
    "--releases",
    "--notes",
    "--issues",
    "--signing-key",
    "--github-only",
    "--pack-ci",
    "--draft-ci",
    "--sign-ci",
    "--output",
];

pub fn command(root: &Path, args: &[String], public_key: &[u8]) -> Result<()> {
    let mut flags = options(args, SWITCHES, VALUES)?;
    let key = flags
        .get("--signing-key")
        .map(PathBuf::from)
        .or_else(|| env::var_os("MINARBOT_RELEASE_SIGNING_KEY").map(PathBuf::from));
    let key = key.unwrap_or_else(|| home().join(".config/minarbot-release/signing-ed25519.pem"));
    let key = files::absolute(&key)?;
    for (mode, allowed) in [
        ("--pack-ci", &["--notes", "--output"][..]),
        ("--draft-ci", &["--notes"][..]),
        ("--sign-ci", &["--signing-key"][..]),
    ] {
        if let Some(value) = flags.get(mode) {
            if flags
                .keys()
                .any(|flag| flag != mode && !allowed.contains(&flag.as_str()))
            {
                return Err("Incompatible CI release options.".into());
            }
            let notes = || flags.get("--notes").cloned().unwrap_or_default();
            return match mode {
                "--pack-ci" => pack_ci(
                    root,
                    &files::absolute(Path::new(value))?,
                    &files::absolute(Path::new(
                        flags.get("--output").map_or("artifacts", String::as_str),
                    ))?,
                    notes(),
                ),
                "--draft-ci" => {
                    github::draft_ci(root, &files::absolute(Path::new(value))?, notes())
                }
                _ => {
                    check_clean(root)?;
                    github::sign_ci(root, value, &key, public_key)
                }
            };
        }
    }
    let releases = flags
        .remove("--releases")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share/minarbot/releases"));
    let releases = files::absolute(&releases)?;
    if let Some(id) = flags.remove("--github-only") {
        if flags
            .keys()
            .any(|flag| !["--signing-key", "--output"].contains(&flag.as_str()))
        {
            return Err("--github-only accepts only --releases and --signing-key.".into());
        }
        check_clean(root)?;
        return github::upload(root, &releases, &id, &key, public_key);
    }
    let allow_dirty = flags.contains_key("--allow-dirty");
    let dry_run = flags.contains_key("--dry-run");
    if dry_run
        && !args
            .iter()
            .any(|arg| arg == "--releases" || arg.starts_with("--releases="))
    {
        return Err("--dry-run requires an explicit --releases directory for its output.".into());
    }
    let github = !dry_run && !allow_dirty && !flags.contains_key("--no-github");
    if !allow_dirty {
        check_clean(root)?;
    } else if !flags.contains_key("--no-github") && !dry_run {
        eprintln!("--allow-dirty publishes locally only.");
    }
    let notes = flags.get("--notes").map(String::as_str);
    if github && notes.is_none_or(|notes| notes.trim().is_empty()) {
        return Err(
            "Pass --notes with user-facing release notes for GitHub, or use --no-github.".into(),
        );
    }
    let image = if github && !flags.contains_key("--no-image") {
        Some(github::ImageCredentials::read(false)?)
    } else {
        None
    };
    let local = Local {
        root,
        scripts_root: root,
        source: &root.join("app"),
        releases: &releases,
        allow_dirty,
        notes,
        issues: parse_issues(flags.get("--issues"))?,
        build: !dry_run,
    };
    if dry_run {
        if releases.exists() && fs::read_dir(&releases)?.next().is_some() {
            return Err("--dry-run requires an empty output directory.".into());
        }
        // ponytail: dry runs compare prebuilt inputs. Run the full gate independently before publication.
        for file in [
            "app/dist/index.html",
            "target/release/minarbot-server",
            "target/release/minarbot-launcher",
        ] {
            if !root.join(file).is_file() {
                return Err(format!("Build {file} before --dry-run.").into());
            }
        }
        leptos::require_build(local.source)?;
    }
    let info = local.build(image)?;
    println!(
        "{} {} locally",
        if dry_run { "Staged" } else { "Published" },
        info.id
    );
    let directory = releases.join(".github").join(&info.id);
    if dry_run {
        files::mkdir(&directory)?;
        files::durable(
            &directory.join("dry-run"),
            b"Prebuilt inputs. Publication is disabled.\n",
        )?;
    }
    let (desktop, tauri) = if flags.contains_key("--desktop") {
        let (legacy, tauri) = build_desktop(
            root,
            &releases.join(&info.id),
            &directory,
            &info.version,
            dry_run,
        )?;
        (Some(legacy), tauri)
    } else {
        (None, BTreeMap::new())
    };
    if (!allow_dirty || dry_run) && notes.is_some_and(|notes| !notes.trim().is_empty()) {
        if !allow_dirty {
            check_head(root, &info.commit)?;
        }
        files::mkdir(&directory)?;
        let ready = Ready {
            release: sha256(&releases.join(&info.id).join("release.json"))?,
            desktop,
            tauri,
        };
        durable(
            &directory.join("ready.json"),
            &serde_json::to_vec(&ready)?,
            0o600,
        )?;
    }
    if dry_run {
        github::preview(&releases, &directory, &info)?;
    } else if github {
        github::upload(root, &releases, &info.id, &key, public_key)?;
    }
    Ok(())
}

fn home() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

#[cfg(test)]
pub fn fixture_build(root: &Path, options: Value) -> Result<Info> {
    let repository = options["repository"].as_str().map_or(root, Path::new);
    let source = options["source"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| repository.join("app"));
    let releases = options["releases"]
        .as_str()
        .ok_or("Missing releases directory")?;
    let issues = match options.get("issues") {
        None => Vec::new(),
        Some(value) => value
            .as_array()
            .ok_or("Invalid issues")?
            .iter()
            .map(|n| {
                n.as_u64()
                    .filter(|n| (1..=9_007_199_254_740_991).contains(n))
                    .ok_or("Invalid issues")
            })
            .collect::<std::result::Result<Vec<_>, _>>()?,
    };
    let mut seen = BTreeSet::new();
    let local = Local {
        root: repository,
        scripts_root: root,
        source: &source,
        releases: Path::new(releases),
        allow_dirty: options["allowDirty"] == true,
        notes: options["notes"].as_str(),
        issues: issues
            .into_iter()
            .filter(|issue| seen.insert(*issue))
            .collect(),
        build: options["build"] != false,
    };
    local.build(None)
}

#[cfg(test)]
pub fn fixture_parse(value: Value) -> Result<Info> {
    if let Some(issues) = value.get("issues") {
        if !issues.is_array()
            || issues
                .as_array()
                .into_iter()
                .flatten()
                .any(|issue| issue.as_u64().is_none())
        {
            return Err("Invalid release issues.".into());
        }
    }
    let info = serde_json::from_value(value)?;
    validate(&info)?;
    Ok(info)
}
