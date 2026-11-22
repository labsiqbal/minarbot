use super::{copy, output, private_stage, run, sha256, Result};
use serde_json::json;
use std::{
    collections::BTreeMap,
    env,
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const VERSION: &str = "0.2.130";
const WASM_PROFILE: &str = "wasm-release";
const BINARYEN_VERSION: &str = "133";
const NAMESPACE: &str = "/_leptos/";
// Each route group gets its own document, CSS and Wasm.
const ROUTE_GROUPS: [(&str, &[&str]); 3] = [
    ("appearance", &[]),
    ("workflows", &["--features", "workflows"]),
    ("conversation", &["--features", "conversation"]),
];

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

fn wasm_opt(target: &Path) -> Result<PathBuf> {
    let (host, checksum) = match (env::consts::OS, env::consts::ARCH) {
        ("linux", "x86_64") => (
            "x86_64-linux",
            "2dc9c7813f5375db93d96ead4b78222fcc3e2677bbb832297af4797782a37489",
        ),
        ("linux", "aarch64") => (
            "aarch64-linux",
            "89c07ea56faf38d0fbecf36ca8ec0721756716185f265b568e133d427f299bf8",
        ),
        ("macos", "x86_64") => (
            "x86_64-macos",
            "13a9b90be775c6389ce3d1f879cb8627bea56708ba8c122983941d53a8199b95",
        ),
        ("macos", "aarch64") => (
            "arm64-macos",
            "ad66da82ac13f163e424b1643f16c6dfcccc98b5966296b43e52d3cab04f84a8",
        ),
        _ => return Err("Unsupported host for pinned Binaryen CLI.".into()),
    };
    let name = format!("binaryen-version_{BINARYEN_VERSION}-{host}");
    let bin = target.join(&name).join("bin/wasm-opt");
    if !bin.is_file() {
        let temp = private_stage("binaryen-cli-", target)?;
        let archive = temp.path().join(format!("{name}.tar.gz"));
        run(Command::new("curl")
            .args(["--fail", "--silent", "--show-error", "--location", "--retry", "2", "--max-time", "180", "--output"])
            .arg(&archive)
            .arg(format!("https://github.com/WebAssembly/binaryen/releases/download/version_{BINARYEN_VERSION}/{name}.tar.gz")))?;
        if sha256(&archive)? != checksum {
            return Err("Binaryen download checksum mismatch.".into());
        }
        let unpacked = format!("binaryen-version_{BINARYEN_VERSION}");
        run(Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(temp.path())
            .arg(format!("{unpacked}/bin/wasm-opt")))?;
        if let Err(error) = fs::rename(temp.path().join(unpacked), target.join(&name)) {
            if !bin.is_file() {
                return Err(error.into());
            }
        }
    }
    let version = output(Command::new(&bin).arg("--version"))?;
    if version.split_whitespace().nth(2) != Some(BINARYEN_VERSION) {
        return Err("Unexpected Binaryen CLI version.".into());
    }
    Ok(bin)
}

fn gzip(source: &Path, destination: &Path) -> Result<u64> {
    run(Command::new("gzip")
        .args(["-n", "-9", "-c"])
        .arg(source)
        .stdout(Stdio::from(File::create(destination)?)))?;
    Ok(fs::metadata(destination)?.len())
}

fn hash_asset(source: &Path, assets: &Path, name: &str, ext: &str) -> Result<String> {
    let hash = sha256(source)?;
    let filename = format!("{name}.{}.{ext}", &hash[..16]);
    copy(source, &assets.join(&filename))?;
    Ok(format!("{NAMESPACE}assets/{filename}"))
}

pub fn require_build(app: &Path) -> Result<()> {
    for (name, _) in ROUTE_GROUPS {
        if !app.join(format!("dist/_leptos/{name}.html")).is_file() {
            return Err(format!(
                "Build the Leptos {name} group with cargo xtask leptos before packaging."
            )
            .into());
        }
    }
    Ok(())
}

pub fn build(root: &Path) -> Result<()> {
    let app = root.join("app");
    let web = root.join("crates/web");
    let target = web.join("target");
    fs::create_dir_all(&target)?;
    let cli = bindgen(&target)?;
    let optimizer = wasm_opt(&target)?;
    let began = Instant::now();
    let dist = app.join("dist");
    fs::create_dir_all(&dist)?;
    let temp = private_stage("bindgen-", &target)?;
    let stage = private_stage(".leptos-", &dist)?;
    let assets = stage.path().join("assets");
    fs::create_dir(&assets)?;
    let mut inputs = BTreeMap::new();
    let mut cargo_time = Duration::ZERO;
    let mut bundles = BTreeMap::new();
    for (name, features) in ROUTE_GROUPS {
        let cargo_began = Instant::now();
        run(Command::new("cargo")
            .current_dir(&web)
            .env_remove("RUSTUP_TOOLCHAIN")
            .env("CARGO_TARGET_DIR", &target)
            .args([
                "build",
                "--locked",
                "--profile",
                WASM_PROFILE,
                "--target",
                "wasm32-unknown-unknown",
                "--config",
                "target.wasm32-unknown-unknown.rustflags=['--cfg','erase_components']",
            ])
            .args(features))?;
        cargo_time += cargo_began.elapsed();
        run(Command::new(&cli)
            .args([
                "--target",
                "web",
                "--out-name",
                name,
                "--no-typescript",
                "--remove-name-section",
                "--remove-producers-section",
                "--out-dir",
            ])
            .arg(temp.path())
            .arg(target.join(format!(
                "wasm32-unknown-unknown/{WASM_PROFILE}/minarbot_web.wasm"
            ))))?;
        let wasm_name = format!("{name}_bg.wasm");
        let unoptimized = temp.path().join(&wasm_name);
        let before_raw = fs::metadata(&unoptimized)?.len();
        let before_gzip = gzip(&unoptimized, &unoptimized.with_extension("wasm.gz"))?;
        let optimized = temp.path().join(format!("{name}_optimized.wasm"));
        run(Command::new(&optimizer)
            .args(["-Oz", "--strip-debug", "--strip-producers", "-o"])
            .arg(&optimized)
            .arg(&unoptimized))?;
        let after_raw = fs::metadata(&optimized)?.len();
        let after_gzip = gzip(&optimized, &optimized.with_extension("wasm.gz"))?;
        let wasm = hash_asset(&optimized, &assets, name, "wasm")?;
        bundles.insert(
            name,
            json!({
                "routeGroup": if name == "appearance" { "shell-settings" } else { name },
                "beforeWasmOpt": { "raw": before_raw, "gzip": before_gzip },
                "afterWasmOpt": { "raw": after_raw, "gzip": after_gzip },
                "asset": wasm,
            }),
        );
        let glue = temp.path().join(format!("{name}.js"));
        fs::write(&glue, fs::read_to_string(&glue)?.replace(&wasm_name, &wasm))?;
        let adapter = match name {
            "conversation" => "globalThis.MinarbotConversation = (await import('minarbot-conversation-adapter')).browserAdapter;",
            "workflows" => "globalThis.minarbotWorkflowMarkdown = (text, scope) => import('minarbot-workflow-markdown').then(({ renderWorkflowMarkdown }) => renderWorkflowMarkdown(text, scope));",
            _ => "",
        };
        let start = temp.path().join(format!("{name}-start.js"));
        fs::write(
            &start,
            fs::read_to_string(web.join("start.js"))?
                .replace("__APP__", name)
                .replace("__MOUNT__", &format!("mount_{name}"))
                .replace("__WASM_URL__", &wasm)
                .replace("__ADAPTER__", adapter),
        )?;
        inputs.insert(format!("{name}-start"), start);
        inputs.insert(name.to_owned(), web.join(format!("{name}.css")));
    }
    let bundle = temp.path().join("bundle");
    run(Command::new("node")
        .current_dir(&app)
        .env("MINARBOT_LEPTOS_INPUTS", serde_json::to_string(&inputs)?)
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
    let entries = fs::read_dir(&assets)?.collect::<std::io::Result<Vec<_>>>()?;
    for entry in entries {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "Invalid asset filename")?;
        let mut size = json!({ "raw": entry.metadata()?.len() });
        if !name.ends_with(".woff2") {
            size["gzip"] = json!(gzip(&entry.path(), &assets.join(format!("{name}.gz")))?);
        }
        sizes.insert(format!("assets/{name}"), size);
    }
    for (name, _) in ROUTE_GROUPS {
        let asset = |prefix: &str, extension: &str| -> Result<String> {
            let key = format!("assets/{prefix}.");
            let path = sizes
                .keys()
                .find(|path| path.starts_with(&key) && path.ends_with(extension))
                .ok_or_else(|| format!("Missing Leptos asset {prefix}{extension}"))?;
            Ok(format!("{NAMESPACE}{path}"))
        };
        let config =
            fs::read_to_string(bundle.join(format!("{name}-config.json")))?.replace('<', "\\u003c");
        let document = format!("{name}.html");
        let html = fs::read_to_string(web.join(&document))?
            .replace("__ICON_URL__", &icon)
            .replace("__STYLES_URL__", &asset(name, ".css")?)
            .replace("__THEMES_URL__", &themes)
            .replace("__START_URL__", &asset(&format!("{name}-start"), ".js")?)
            .replace("__CONFIG__", &config);
        fs::write(stage.path().join(document), html)?;
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
            "packageSeconds": began.elapsed().saturating_sub(cargo_time).as_secs_f64(),
            "wasmProfile": WASM_PROFILE,
            "binaryen": BINARYEN_VERSION,
            "bundles": bundles,
            "sizes": sizes,
        }))?
    );
    Ok(())
}
