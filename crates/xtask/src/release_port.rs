//! Release builder cases ported from `app/updates.test.ts` (#89).
//!
//! The Node suite reached this code through the `app_fixture` runner; these
//! tests call the same functions directly. Release gates still run the
//! package scripts through `npm`, which is what the product does, so the
//! fixture scripts are plain shell commands instead of `node -e`.

use super::{release, Result};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!("updates-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `nativeReleaseFixture`: the launcher contract fixture as `bin/minarbot-server`.
fn native(directory: &Path, version: &str) -> Result<()> {
    fs::create_dir_all(directory.join("bin"))?;
    let binary = directory.join("bin/minarbot-server");
    let status = Command::new("rustc")
        .arg(repository().join("crates/minarbot-launcher/tests/fixtures/contract.rs"))
        .args(["-O", "-o"])
        .arg(&binary)
        .env("MINARBOT_FIXTURE_VERSION", version)
        .env("MINARBOT_FIXTURE_MODE", "healthy")
        .status()?;
    if !status.success() {
        return Err("rustc contract fixture failed".into());
    }
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// The Node `fixture(source, version)`.
fn fixture(source: &Path, version: &str) -> Result<()> {
    fs::create_dir_all(source.join("dist/_leptos"))?;
    fs::write(
        source.join("dist/_leptos/conversation.html"),
        "<title>Minarbot fixture</title>",
    )?;
    fs::write(
        source.join("package.json"),
        json!({"name": "fixture", "version": version, "type": "module"}).to_string(),
    )?;
    native(source, version)
}

fn git(directory: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .current_dir(directory)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if !status.success() {
        return Err(format!("git {args:?} failed").into());
    }
    Ok(())
}

fn commit_repository(directory: &Path) -> Result<()> {
    git(directory, &["init"])?;
    git(
        directory,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "Fixture",
        ],
    )
}

fn build(options: Value) -> Result<release::Info> {
    release::fixture_build(&repository(), options)
}

fn latest(releases: &Path) -> Result<Option<String>> {
    let path = releases.join("feed/latest.json");
    if !path.is_file() {
        return Ok(None);
    }
    let value = release::fixture_parse(serde_json::from_slice(&fs::read(path)?)?)?;
    Ok(Some(value.id))
}

fn err_text<T>(result: Result<T>) -> Result<String> {
    match result {
        Ok(_) => Err("expected an error".into()),
        Err(error) => Ok(error.to_string()),
    }
}

#[test]
fn release_builder_publishes_verified_immutable_files_before_feed_metadata_and_refuses_dirty_trees(
) -> Result<()> {
    let dir = Scratch::new()?;
    let source = dir.0.join("source");
    let releases = dir.0.join("releases");
    fixture(&source, "1.0.0")?;
    fs::write(source.join(".env"), "NEVER_COPY_SECRET")?;
    let options = |extra: Value| {
        let mut base = json!({"source": source, "releases": releases, "build": false,
            "install": false, "allowDirty": true, "notes": "Native check"});
        if let (Some(base), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                base.insert(key.clone(), value.clone());
            }
            base.retain(|_, value| !value.is_null());
        }
        base
    };
    let info = build(options(json!({})))?;
    if release::verify(&releases, &info.id)?.version != "1.0.0" {
        return Err("verified version".into());
    }
    if latest(&releases)?.as_deref() != Some(info.id.as_str()) {
        return Err("feed does not point at the new release".into());
    }
    if releases.join(&info.id).join(".env").exists() {
        return Err(".env was copied into the release".into());
    }
    if !info.files.contains_key("bin/minarbot-server") {
        return Err("native server missing from the manifest".into());
    }
    if info.files.contains_key("release.json") {
        return Err("release.json is listed in its own manifest".into());
    }
    let repository = dir.0.join("dirty-repository");
    fs::create_dir_all(&repository)?;
    commit_repository(&repository)?;
    fs::write(repository.join("dirty"), "fixture")?;
    let dirty = err_text(build(options(
        json!({"repository": repository, "allowDirty": null, "notes": null}),
    )))?;
    if !dirty.contains("dirty") {
        return Err(format!("dirty tree error: {dirty}").into());
    }
    let again = err_text(build(options(json!({"notes": null}))))?;
    if !again.contains("already exists") {
        return Err(format!("duplicate release error: {again}").into());
    }
    if latest(&releases)?.as_deref() != Some(info.id.as_str()) {
        return Err("refused builds moved the feed".into());
    }
    fixture(&source, "1.0.1")?;
    symlink("index.html", source.join("dist/unsafe"))?;
    let unsafe_file = err_text(build(options(json!({"notes": null}))))?;
    if !unsafe_file.to_lowercase().contains("unsupported") {
        return Err(format!("symlink error: {unsafe_file}").into());
    }
    if latest(&releases)?.as_deref() != Some(info.id.as_str()) {
        return Err("unsupported file moved the feed".into());
    }
    for entry in fs::read_dir(&releases)? {
        if entry?
            .file_name()
            .to_string_lossy()
            .starts_with(".staging-")
        {
            return Err("staging directory left behind".into());
        }
    }
    fs::write(
        releases.join(&info.id).join("bin/minarbot-server"),
        "tampered",
    )?;
    let tampered = err_text(release::verify(&releases, &info.id))?;
    if !tampered.contains("SHA-256") {
        return Err(format!("tampered release error: {tampered}").into());
    }
    Ok(())
}

fn scripts(source: &Path, lint: &str) -> Result<()> {
    let file = source.join("package.json");
    let mut package: Value = serde_json::from_slice(&fs::read(&file)?)?;
    package["scripts"] = json!({
        "lint": lint,
        "build": "echo yes > built",
        "test": "test -f built || exit 8; echo yes > tested",
    });
    fs::write(file, package.to_string())?;
    Ok(())
}

#[test]
fn release_lint_failure_stops_build_tests_and_packaging_successful_gates_run_in_order() -> Result<()>
{
    let dir = Scratch::new()?;
    let source = dir.0.join("source");
    let releases = dir.0.join("releases");
    fixture(&source, "1.0.0")?;
    commit_repository(&dir.0)?;
    scripts(&source, "exit 7")?;
    let options = json!({"source": source, "releases": releases, "repository": dir.0,
        "install": false, "allowDirty": true});
    let failed = err_text(build(options.clone()))?;
    if !failed.contains("Command failed") {
        return Err(format!("lint failure: {failed}").into());
    }
    if source.join("tested").exists() {
        return Err("tests ran after a lint failure".into());
    }
    if source.join("built").exists() {
        return Err("build ran after a lint failure".into());
    }
    if releases.exists() {
        return Err("a release was packaged after a lint failure".into());
    }
    scripts(&source, "true")?;
    build(options)?;
    if !source.join("built").exists() {
        return Err("build gate did not run".into());
    }
    if !source.join("tested").exists() {
        return Err("test gate did not run after build".into());
    }
    Ok(())
}

#[test]
fn desktop_build_follows_dirty_guard_and_packages_release_version_without_changing_tracked_desktop_version(
) -> Result<()> {
    let dir = Scratch::new()?;
    let root = &dir.0.join("repo");
    for folder in ["app", "scripts", "desktop/tauri"] {
        fs::create_dir_all(root.join(folder))?;
    }
    for name in ["install-machine.sh", "uninstall-machine.sh"] {
        fs::copy(
            repository().join("scripts").join(name),
            root.join("scripts").join(name),
        )?;
    }
    let config = "desktop/tauri/tauri.conf.json";
    let mut desktop: Value = serde_json::from_slice(&fs::read(repository().join(config))?)?;
    let app = json!({"name": "release-fixture", "version": "1.0.1", "type": "module",
        "scripts": {"lint": "true", "test": "true", "build": "true"}});
    fs::write(root.join("app/package.json"), app.to_string())?;
    fs::write(
        root.join("app/package-lock.json"),
        json!({"name": app["name"], "version": app["version"], "lockfileVersion": 3,
            "packages": {"": app}})
        .to_string(),
    )?;
    native(&root.join("app"), "1.0.1")?;
    fs::create_dir_all(root.join("app/dist/_leptos"))?;
    fs::write(root.join("app/dist/_leptos/conversation.html"), "fixture")?;
    desktop["version"] = json!("1.0.0");
    fs::write(root.join(config), desktop.to_string())?;
    git(root, &["init"])?;
    git(root, &["add", "."])?;
    git(
        root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-m",
            "Fixture",
        ],
    )?;
    let releases = dir.0.join("releases");
    let args: Vec<String> = [
        "--desktop",
        "--no-github",
        "--notes",
        "Fixture",
        "--signing-key",
        "unused.pem",
        "--releases",
    ]
    .iter()
    .map(|arg| (*arg).to_owned())
    .chain([releases.to_string_lossy().into_owned()])
    .collect();
    fs::write(root.join("dirty"), "uncommitted")?;
    let dirty = err_text(release::command_with_desktop(
        root,
        &args,
        &minarbot_launcher::RELEASE_PUBLIC_KEY,
        desktop_fixture,
    ))?;
    if !dirty.contains("dirty worktree") {
        return Err(format!("dirty desktop release: {dirty}").into());
    }
    if releases.exists() {
        return Err("dirty desktop release wrote releases".into());
    }
    fs::remove_file(root.join("dirty"))?;
    release::command_with_desktop(
        root,
        &args,
        &minarbot_launcher::RELEASE_PUBLIC_KEY,
        desktop_fixture,
    )?;
    let id = latest(&releases)?.ok_or("no latest release")?;
    let info = release::verify(&releases, &id)?;
    if info.version != "1.0.1" {
        return Err(format!("release version {}", info.version).into());
    }
    let directory = releases.join(".github").join(&info.id);
    let ready: release::Ready = serde_json::from_slice(&fs::read(directory.join("ready.json"))?)?;
    let platform = format!("linux-{}", std::env::consts::ARCH);
    let [_, image, _] = super::desktop::tauri_names("1.0.1", &platform);
    assert_eq!(fs::read_to_string(directory.join(image))?, "1.0.1");
    super::desktop::verify_feed(&directory, "1.0.1", &platform)?;
    assert_eq!(
        ready.release,
        super::sha256(&releases.join(&info.id).join("release.json"))?
    );
    let older = json!({
        "release": ready.release, "desktop": ["old-image", "old-feed"], "tauri": {}
    });
    assert!(serde_json::from_value::<release::Ready>(older).is_err());
    assert_eq!(ready.tauri.len(), 3);
    for (name, hash) in ready.tauri {
        assert_eq!(hash, super::sha256(&directory.join(name))?);
    }
    let tracked: Value = serde_json::from_slice(&fs::read(root.join(config))?)?;
    if tracked["version"] != "1.0.0" {
        return Err(format!("tracked desktop version changed: {}", tracked["version"]).into());
    }
    Ok(())
}

fn desktop_fixture(
    _: &Path,
    runtime: &Path,
    output: &Path,
    version: &str,
    dry: bool,
) -> Result<[String; 3]> {
    assert!(!dry);
    assert!(fs::metadata(runtime)?.len() > 0);
    let names = super::desktop::tauri_names(version, &format!("linux-{}", std::env::consts::ARCH));
    let [signature, artifact, feed] = &names;
    fs::write(output.join(signature), "fixture signature")?;
    fs::write(output.join(artifact), version)?;
    fs::write(
        output.join(feed),
        json!({
            "version": version,
            "url": format!("https://github.com/labsiqbal/minarbot/releases/download/v{version}/{artifact}"),
            "signature": "fixture signature",
        })
        .to_string(),
    )?;
    Ok(names)
}
