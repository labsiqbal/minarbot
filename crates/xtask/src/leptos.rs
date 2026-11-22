use super::{copy, output, private_stage, run, sha256, Result};
use serde_json::json;
use std::{
    collections::BTreeMap,
    env,
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};

const VERSION: &str = "0.2.130";
const NAMESPACE: &str = "/_leptos/";

fn bindgen(target: &Path) -> Result<PathBuf> {
    let (host, checksum) = match (env::consts::OS, env::consts::ARCH) {
        ("linux", "x86_64") => (
            "x86_64-unknown-linux-musl",
            "9687f3bb551b9f66f37ed32a40d7913a00f7962db653054e56857bc306c3227f",
        ),
        ("linux", "aarch64") => (
            "aarch64-unknown-linux-musl",
            "99ffc93ce80081b91632e2b539b782821a8f3439ab0fddc645287462b2f55d3d",
        ),
        ("macos", "x86_64") => (
            "x86_64-apple-darwin",
            "eb209c115b7cb95832423c1aa3f4d06d6c53f63ef74fc4b6fb79ef0b2eea5f10",
        ),
        ("macos", "aarch64") => (
            "aarch64-apple-darwin",
            "b90ddb9357f584b1af897d58ad77e4cce2b48fc17e2a967bc249a87f72809444",
        ),
        _ => return Err("Unsupported host for pinned wasm-bindgen CLI.".into()),
    };
    let name = format!("wasm-bindgen-{VERSION}-{host}");
    let bin = target.join(&name).join("wasm-bindgen");
    if !bin.is_file() {
        let temp = private_stage("bindgen-cli-", target)?;
        let archive = temp.path().join(format!("{name}.tar.gz"));
        run(Command::new("curl")
            .args(["--fail", "--silent", "--show-error", "--location", "--retry", "2", "--max-time", "90", "--output"])
            .arg(&archive)
            .arg(format!("https://github.com/wasm-bindgen/wasm-bindgen/releases/download/{VERSION}/{name}.tar.gz")))?;
        if sha256(&archive)? != checksum {
            return Err("wasm-bindgen download checksum mismatch.".into());
        }
        run(Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(temp.path()))?;
        if let Err(error) = fs::rename(temp.path().join(&name), target.join(&name)) {
            if !bin.is_file() {
                return Err(error.into());
            }
        }
    }
    if output(Command::new(&bin).arg("--version"))?.trim() != format!("wasm-bindgen {VERSION}") {
        return Err("Unexpected wasm-bindgen CLI version.".into());
    }
    Ok(bin)
}

fn hash_asset(source: &Path, assets: &Path, name: &str, ext: &str) -> Result<String> {
    let hash = sha256(source)?;
    let filename = format!("{name}.{}.{ext}", &hash[..16]);
    copy(source, &assets.join(&filename))?;
    Ok(format!("{NAMESPACE}assets/{filename}"))
}

pub fn require_build(app: &Path) -> Result<()> {
    if !app.join("dist/_leptos/appearance.html").is_file() {
        return Err("Build Leptos Appearance with cargo xtask leptos before packaging.".into());
    }
    Ok(())
}

pub fn build(root: &Path) -> Result<()> {
    let app = root.join("app");
    let web = root.join("crates/web");
    let target = web.join("target");
    fs::create_dir_all(&target)?;
    let cli = bindgen(&target)?;
    let began = Instant::now();
    run(Command::new("cargo")
        .current_dir(&web)
        .env_remove("RUSTUP_TOOLCHAIN")
        .env("CARGO_TARGET_DIR", &target)
        .args([
            "build",
            "--locked",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
        ]))?;
    let cargo_time = began.elapsed();
    let package_began = Instant::now();
    let dist = app.join("dist");
    fs::create_dir_all(&dist)?;
    let temp = private_stage("bindgen-", &target)?;
    let stage = private_stage(".leptos-", &dist)?;
    let assets = stage.path().join("assets");
    fs::create_dir(&assets)?;
    run(Command::new(&cli)
        .args([
            "--target",
            "web",
            "--out-name",
            "appearance",
            "--no-typescript",
            "--out-dir",
        ])
        .arg(temp.path())
        .arg(target.join("wasm32-unknown-unknown/release/minarbot_web.wasm")))?;
    let wasm = hash_asset(
        &temp.path().join("appearance_bg.wasm"),
        &assets,
        "appearance",
        "wasm",
    )?;
    let glue = temp.path().join("appearance.js");
    fs::write(
        &glue,
        fs::read_to_string(&glue)?.replace("appearance_bg.wasm", &wasm),
    )?;
    let start_source = temp.path().join("start.js");
    fs::write(
        &start_source,
        fs::read_to_string(web.join("start.js"))?.replace("__WASM_URL__", &wasm),
    )?;
    let bundle = temp.path().join("bundle");
    run(Command::new("node")
        .current_dir(&app)
        .env("MINARBOT_LEPTOS_START", start_source)
        .args([
            "node_modules/vite/bin/vite.js",
            "build",
            "--config",
            "vite.leptos.config.ts",
            "--outDir",
        ])
        .arg(&bundle))?;
    copy(&bundle.join("assets"), &assets)?;
    let themes = hash_asset(&bundle.join("themes.css"), &assets, "themes", "css")?;
    let icon = hash_asset(&app.join("src/favicon.svg"), &assets, "minarbot", "svg")?;
    let mut sizes = BTreeMap::new();
    let (mut styles, mut conversation_styles, mut start) = (None, None, None);
    let entries = fs::read_dir(&assets)?.collect::<std::io::Result<Vec<_>>>()?;
    for entry in entries {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "Invalid asset filename")?;
        let url = || format!("{NAMESPACE}assets/{name}");
        if name.starts_with("appearance.") && name.ends_with(".css") {
            styles = Some(url());
        }
        if name.starts_with("conversation.") && name.ends_with(".css") {
            conversation_styles = Some(url());
        }
        if name.starts_with("start.") && name.ends_with(".js") {
            start = Some(url());
        }
        let mut size = json!({ "raw": entry.metadata()?.len() });
        if !name.ends_with(".woff2") {
            let gzip = assets.join(format!("{name}.gz"));
            run(Command::new("gzip")
                .args(["-n", "-9", "-c"])
                .arg(entry.path())
                .stdout(Stdio::from(File::create(&gzip)?)))?;
            size["gzip"] = json!(fs::metadata(gzip)?.len());
        }
        sizes.insert(format!("assets/{name}"), size);
    }
    let config = fs::read_to_string(bundle.join("appearance-config.json"))?.replace('<', "\\u003c");
    let start = start.ok_or("Leptos bootstrap is missing.")?;
    for (name, styles) in [
        ("appearance.html", styles),
        ("conversation.html", conversation_styles),
    ] {
        let html = fs::read_to_string(web.join(name))?
            .replace("__ICON_URL__", &icon)
            .replace("__STYLES_URL__", &styles.ok_or("Leptos CSS is missing.")?)
            .replace("__THEMES_URL__", &themes)
            .replace("__START_URL__", &start)
            .replace("__CONFIG__", &config);
        fs::write(stage.path().join(name), html)?;
    }
    for name in ["NOTICE", "NOTICE.hermes", "NOTICE.t3", "provenance.json"] {
        copy(
            &app.join("src/ui").join(name),
            &stage.path().join(format!("ui.{name}")),
        )?;
    }
    for (source, name) in [
        (
            app.join("node_modules/lucide-react/LICENSE"),
            "lucide.LICENSE",
        ),
        (
            app.join("node_modules/@fontsource-variable/inter/LICENSE"),
            "inter.LICENSE",
        ),
        (web.join("NOTICE"), "NOTICE"),
        (web.join("leptos.LICENSE"), "leptos.LICENSE"),
    ] {
        copy(&source, &stage.path().join(name))?;
    }
    let cli_dir = cli.parent().ok_or("Missing wasm-bindgen directory")?;
    for name in ["LICENSE-MIT", "LICENSE-APACHE"] {
        copy(
            &cli_dir.join(name),
            &stage.path().join(format!("wasm-bindgen.{name}")),
        )?;
    }
    let destination = dist.join("_leptos");
    if destination.exists() {
        fs::remove_dir_all(&destination)?;
    }
    fs::rename(stage.path(), destination)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "cargoWallSeconds": cargo_time.as_secs_f64(),
            "packageSeconds": package_began.elapsed().as_secs_f64(),
            "sizes": sizes,
        }))?
    );
    Ok(())
}
