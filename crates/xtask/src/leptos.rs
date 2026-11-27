use super::{copy, output, private_stage, run, sha256, Result};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const VERSION: &str = "0.2.130";
const WASM_PROFILE: &str = "wasm-release";
/// `cargo xtask leptos --dev`: incremental, without LTO, `wasm-opt` or
/// size budgets, for trying UI changes. Releases use [`WASM_PROFILE`].
const WASM_DEV_PROFILE: &str = "wasm-dev";
const BINARYEN_VERSION: &str = "133";
const NAMESPACE: &str = "/_leptos/";
// Each route group gets its own document, CSS and Wasm.
const ROUTE_GROUPS: [(&str, &[&str], u64); 4] = [
    ("appearance", &[], 960_000),
    ("workflows", &["--features", "workflows"], 610_000),
    ("conversation", &["--features", "conversation"], 430_000),
    ("hub", &["--features", "hub"], 240_000),
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
        // A cache can restore the tool directory without its binary.
        if target.join(&name).exists() {
            fs::remove_dir_all(target.join(&name))?;
        }
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
        // A cache can restore the tool directory without its binary.
        if target.join(&name).exists() {
            fs::remove_dir_all(target.join(&name))?;
        }
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

fn optimize(optimizer: &Path, input: &Path, output: &Path) -> Result<()> {
    if optimizer.as_os_str().is_empty() {
        fs::copy(input, output)?;
        return Ok(());
    }
    run(Command::new(optimizer)
        .args(["-Oz", "--strip-debug", "--strip-producers", "-o"])
        .arg(output)
        .arg(input))
}

fn split_wasm(
    input: &Path,
    temp: &Path,
    assets: &Path,
    optimizer: &Path,
    name: &str,
) -> Result<(PathBuf, BTreeMap<String, Vec<String>>)> {
    let bytes = fs::read(input)?;
    let directory = temp.join(format!("{name}-split"));
    let primary = directory.join("main.wasm");
    let link = format!("./{name}-chunks.js");
    let module = format!("./{name}.js");
    let mut opts = wasm_split_cli_support::Options::new(&bytes);
    opts.output_dir = &directory;
    opts.main_out_path = &primary;
    opts.link_name = &link;
    opts.main_module = &module;
    opts.emit_dwarf = false;
    let split = wasm_split_cli_support::transform(opts)
        .map_err(|error| format!("Split Leptos {name}: {error:#}"))?;
    let glue = directory.join(&link);
    let mut js = fs::read_to_string(&glue)?;
    let mut urls = BTreeMap::new();
    for chunk in split.split_modules {
        let stem = chunk
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or("Invalid split module name.")?;
        let optimized = chunk.with_extension("opt.wasm");
        optimize(optimizer, &chunk, &optimized)?;
        let url = hash_asset(&optimized, assets, &format!("{name}-{stem}"), "wasm")?;
        let reference = format!("fetch(new URL(\"./{stem}.wasm\", import.meta.url))");
        if !js.contains(&reference) {
            return Err(format!("Missing URL in {name} loader for {stem}.").into());
        }
        js = js.replace(
            &reference,
            &format!("fetch({url:?}, {{ signal: AbortSignal.timeout(8000) }})"),
        );
        urls.insert(stem.to_owned(), url);
    }
    fs::write(temp.join(link), js)?;
    let prefetches = split
        .prefetch_map
        .into_iter()
        .map(|(label, stems)| {
            let assets = stems
                .into_iter()
                .map(|stem| {
                    urls.get(&stem)
                        .cloned()
                        .ok_or("Missing split module asset.")
                })
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok((label, assets))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    Ok((primary, prefetches))
}

fn hub_assets(
    manifest: &serde_json::Value,
    entries: [&str; 2],
    shared: [&str; 3],
) -> Result<BTreeSet<String>> {
    let manifest = manifest.as_object().ok_or("Invalid Leptos manifest.")?;
    let mut files = BTreeSet::new();
    let mut pending = entries
        .into_iter()
        .map(|path| {
            manifest
                .iter()
                .find(|(_, entry)| entry["file"].as_str() == path.strip_prefix(NAMESPACE))
                .map(|(key, _)| key.as_str())
                .ok_or("Missing hub asset in Leptos manifest.")
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut visited = BTreeSet::new();
    while let Some(key) = pending.pop() {
        if !visited.insert(key) {
            continue;
        }
        let entry = manifest
            .get(key)
            .ok_or("Missing hub dependency in Leptos manifest.")?;
        let file = entry["file"].as_str().ok_or("Missing hub build file.")?;
        files.insert(format!("{NAMESPACE}{file}"));
        for name in ["css", "assets"] {
            for file in entry[name].as_array().into_iter().flatten() {
                let file = file.as_str().ok_or("Invalid hub build file.")?;
                files.insert(format!("{NAMESPACE}{file}"));
            }
        }
        for name in ["imports", "dynamicImports"] {
            for entry in entry[name].as_array().into_iter().flatten() {
                pending.push(entry.as_str().ok_or("Invalid hub build dependency.")?);
            }
        }
    }
    files.extend(shared.map(str::to_owned));
    Ok(files)
}

pub fn require_build(app: &Path) -> Result<()> {
    for (name, _, _) in ROUTE_GROUPS {
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
    build_profile(root, false)
}

pub fn build_dev(root: &Path) -> Result<()> {
    build_profile(root, true)
}

fn build_profile(root: &Path, dev: bool) -> Result<()> {
    let app = root.join("app");
    let web = root.join("crates/web");
    let target = web.join("target");
    fs::create_dir_all(&target)?;
    let cli = bindgen(&target)?;
    // An empty optimizer path copies modules unoptimized.
    let optimizer = if dev {
        PathBuf::new()
    } else {
        wasm_opt(&target)?
    };
    let profile = if dev { WASM_DEV_PROFILE } else { WASM_PROFILE };
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
    let cargo = |name: &str, features: &[&str], target: &Path| {
        let rustflags = if name == "hub" {
            "target.wasm32-unknown-unknown.rustflags=['--cfg','erase_components']"
        } else {
            "target.wasm32-unknown-unknown.rustflags=['--cfg','erase_components','-Clink-arg=--emit-relocs']"
        };
        let mut command = Command::new("cargo");
        command
            .current_dir(&web)
            .env_remove("RUSTUP_TOOLCHAIN")
            .env("CARGO_TARGET_DIR", target)
            .args([
                "build",
                "--locked",
                "--profile",
                profile,
                "--target",
                "wasm32-unknown-unknown",
                "--config",
                rustflags,
            ])
            .args(features);
        command
    };
    // Dev builds give each route group its own target directory and build
    // them at once; release builds share one and run in turn.
    let group_target = |name: &str| {
        if dev {
            target.join(format!("dev-{name}"))
        } else {
            target.clone()
        }
    };
    if dev {
        let cargo_began = Instant::now();
        let children = ROUTE_GROUPS
            .iter()
            .map(|(name, features, _)| {
                Ok((*name, cargo(name, features, &group_target(name)).spawn()?))
            })
            .collect::<Result<Vec<_>>>()?;
        for (name, mut child) in children {
            if !child.wait()?.success() {
                return Err(format!("Leptos {name} dev build failed.").into());
            }
        }
        cargo_time += cargo_began.elapsed();
    }
    for (name, features, budget) in ROUTE_GROUPS {
        if !dev {
            let cargo_began = Instant::now();
            run(&mut cargo(name, features, &target))?;
            cargo_time += cargo_began.elapsed();
        }
        let input = group_target(name).join(format!(
            "wasm32-unknown-unknown/{profile}/minarbot_web.wasm"
        ));
        let (input, chunks) = if name == "hub" {
            (input, BTreeMap::new())
        } else {
            split_wasm(&input, temp.path(), &assets, &optimizer, name)?
        };
        let mut bindgen = Command::new(&cli);
        if name != "hub" {
            // Split modules share the primary module's indirect function table.
            bindgen.arg("--keep-lld-exports");
        }
        run(bindgen
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
            .arg(&input))?;
        let wasm_name = format!("{name}_bg.wasm");
        let unoptimized = temp.path().join(&wasm_name);
        let before_raw = fs::metadata(&unoptimized)?.len();
        let before_gzip = gzip(&unoptimized, &unoptimized.with_extension("wasm.gz"))?;
        let optimized = temp.path().join(format!("{name}_optimized.wasm"));
        optimize(&optimizer, &unoptimized, &optimized)?;
        let after_raw = fs::metadata(&optimized)?.len();
        let after_gzip = gzip(&optimized, &optimized.with_extension("wasm.gz"))?;
        if !dev && after_gzip > budget {
            return Err(format!(
                "Leptos {name} Wasm is {after_gzip} gzip bytes (budget {budget})."
            )
            .into());
        }
        let wasm = hash_asset(&optimized, &assets, name, "wasm")?;
        if name == "workflows" {
            fs::write(
                temp.path().join("route-chunks.js"),
                fs::read_to_string(web.join("route-chunks.js"))?
                    .replace("__WORKFLOWS_WASM__", &wasm),
            )?;
        }
        bundles.insert(
            name,
            json!({
                "routeGroup": if name == "appearance" { "shell-settings" } else { name },
                "beforeWasmOpt": { "raw": before_raw, "gzip": before_gzip },
                "afterWasmOpt": { "raw": after_raw, "gzip": after_gzip },
                "asset": wasm,
                "gzipBudget": budget,
                "chunks": chunks,
            }),
        );
        let glue = temp.path().join(format!("{name}.js"));
        fs::write(&glue, fs::read_to_string(&glue)?.replace(&wasm_name, &wasm))?;
        let adapter = match name {
            "conversation" => {
                "const [adapter, routes] = await Promise.all([
                    import('minarbot-conversation-adapter'), import('./route-chunks.js')
                ]);
                globalThis.MinarbotConversation = adapter.browserAdapter;
                globalThis.minarbotPreloadWorkflows = routes.preloadWorkflows;
                globalThis.minarbotMountRoute = routes.mountRoute;"
            }
            "workflows" => {
                "const chunks = await import('./route-chunks.js');
                globalThis.minarbotWorkflowMarkdown = chunks.workflowMarkdown;
                globalThis.minarbotMountRoute = chunks.mountRoute;"
            }
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
    let manifest = serde_json::from_slice::<serde_json::Value>(&fs::read(
        bundle.join(".vite/manifest.json"),
    )?)?;
    let themes = hash_asset(&bundle.join("themes.css"), &assets, "themes", "css")?;
    let icon = hash_asset(&web.join("assets/favicon.svg"), &assets, "minarbot", "svg")?;
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
    for (name, _, _) in ROUTE_GROUPS {
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
        let styles = asset(name, ".css")?;
        let start = asset(&format!("{name}-start"), ".js")?;
        if name == "hub" {
            let wasm = asset(name, ".wasm")?;
            let mut files = hub_assets(&manifest, [&styles, &start], [&themes, &icon, &wasm])?;
            let css = fs::read_to_string(stage.path().join(styles.trim_start_matches(NAMESPACE)))?;
            files.extend(
                sizes
                    .keys()
                    .filter(|name| name.ends_with(".woff2"))
                    .map(|name| format!("{NAMESPACE}{name}"))
                    .filter(|path| css.contains(path)),
            );
            fs::write(
                stage.path().join("hub-assets.json"),
                serde_json::to_vec(&files)?,
            )?;
        }
        let html = fs::read_to_string(web.join(&document))?
            .replace("__ICON_URL__", &icon)
            .replace("__STYLES_URL__", &styles)
            .replace("__THEMES_URL__", &themes)
            .replace("__START_URL__", &start)
            .replace("__CONFIG__", &config);
        fs::write(stage.path().join(document), html)?;
    }
    for name in ["NOTICE", "NOTICE.hermes", "NOTICE.t3", "provenance.json"] {
        copy(
            &web.join("assets/ui").join(name),
            &stage.path().join(format!("ui.{name}")),
        )?;
    }
    for (source, name) in [
        (web.join("assets/lucide.LICENSE"), "lucide.LICENSE"),
        (web.join("assets/inter/LICENSE"), "inter.LICENSE"),
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
    copy(&web.join("public"), &dist)?;
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
            "wasmProfile": profile,
            "binaryen": BINARYEN_VERSION,
            "bundles": bundles,
            "sizes": sizes,
        }))?
    );
    Ok(())
}
