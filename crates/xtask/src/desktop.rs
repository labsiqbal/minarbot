use base64::{engine::general_purpose::STANDARD, Engine};
use minisign_verify::{PublicKey, Signature};
use semver::Version;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::Command,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const CLI: &str = "@tauri-apps/cli@2.12.6";
const GITHUB: &str = "https://github.com/labsiqbal/minarbot/releases";
#[cfg(unix)]
pub const PLATFORMS: [&str; 5] = [
    "linux-x86_64",
    "linux-aarch64",
    "darwin-x86_64",
    "darwin-aarch64",
    "windows-x86_64",
];

struct Fixture {
    key: PathBuf,
    feed: url::Url,
    port: u16,
}

fn platform() -> Result<&'static str> {
    match (env::consts::OS, env::consts::ARCH) {
        ("linux", "x86_64") => Ok("linux-x86_64"),
        ("linux", "aarch64") => Ok("linux-aarch64"),
        ("macos", "x86_64") => Ok("darwin-x86_64"),
        ("macos", "aarch64") => Ok("darwin-aarch64"),
        ("windows", "x86_64") => Ok("windows-x86_64"),
        _ => Err("Unsupported Minarbot Desktop target.".into()),
    }
}

pub fn tauri_names(version: &str, platform: &str) -> [String; 3] {
    let extension = if platform.starts_with("linux-") {
        "AppImage"
    } else if platform.starts_with("darwin-") {
        "dmg"
    } else {
        "exe"
    };
    let name = format!("Minarbot-Tauri-{version}-{platform}.{extension}");
    [
        format!("{name}.sig"),
        name,
        format!("tauri-latest-{platform}.json"),
    ]
}

fn run(command: &mut Command) -> Result<()> {
    // Report program and exit status only. Signing credentials live in the environment.
    let status = command.env_remove("MINARBOT_GHCR_TOKEN").status()?;
    if !status.success() {
        return Err(format!("{:?} failed ({status}).", command.get_program()).into());
    }
    Ok(())
}

fn cli(root: &Path) -> Command {
    let mut command = Command::new(if cfg!(windows) { "npx.cmd" } else { "npx" });
    command
        .current_dir(root.join("desktop/tauri"))
        .args(["--yes", CLI]);
    command
}

fn electron(root: &Path, runtime: &Path, output: &Path, version: &str, dry: bool) -> Result<()> {
    let mut command = Command::new("node");
    command
        .current_dir(root)
        .arg("desktop/scripts/build.mjs")
        .arg(version)
        .arg(output);
    if cfg!(windows) {
        command.env("MINARBOT_LINUX_ARCHIVE", runtime);
    }
    for name in [
        "TAURI_SIGNING_PRIVATE_KEY",
        "TAURI_SIGNING_PRIVATE_KEY_PASSWORD",
        "TAURI_SIGNING_PRIVATE_KEY_PATH",
    ] {
        command.env_remove(name);
    }
    if dry {
        for name in [
            "CSC_LINK",
            "CSC_KEY_PASSWORD",
            "WIN_CSC_LINK",
            "WIN_CSC_KEY_PASSWORD",
        ] {
            command.env_remove(name);
        }
        command.env("CSC_IDENTITY_AUTO_DISCOVERY", "false");
    }
    run(&mut command)
}

fn append_extension(path: &Path, extension: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(extension);
    name.into()
}

fn find_bundle(directory: &Path, extension: &str, version: &str) -> Result<PathBuf> {
    let mut paths = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    paths.retain(|path| {
        path.extension().is_some_and(|ext| ext == extension)
            && path.is_file()
            && path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().contains(&format!("_{version}_")))
    });
    match paths.as_slice() {
        [file] => Ok(file.clone()),
        _ => Err(format!(
            "Expected one .{extension} bundle in {}.",
            directory.display()
        )
        .into()),
    }
}

fn decoded(text: &str) -> Result<String> {
    Ok(String::from_utf8(STANDARD.decode(text.trim())?)?)
}

fn assert_signature(public: &str, artifact: &Path, signature: &str, version: &str) -> Result<()> {
    let verifier = PublicKey::decode(&decoded(public)?)?;
    let signature = Signature::decode(&decoded(signature)?)?;
    if !signature
        .trusted_comment()
        .split('\t')
        .any(|field| field == format!("version:{version}"))
    {
        return Err("Missing signed desktop version.".into());
    }
    verifier.verify(&fs::read(artifact)?, &signature, false)?;
    Ok(())
}

fn build_tauri(
    root: &Path,
    runtime: &Path,
    output: &Path,
    version: &str,
    fixture: Option<&Fixture>,
) -> Result<()> {
    let platform = platform()?;
    let bundle = match env::consts::OS {
        "linux" => "appimage",
        "macos" => "dmg",
        _ => "nsis",
    };
    let [signature_name, artifact_name, feed_name] = tauri_names(version, platform);
    let target = env::var_os("MINARBOT_TAURI_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("desktop/tauri/target"));
    let target = std::path::absolute(target)?;
    let debug = fixture.is_some() && env::var_os("MINARBOT_TAURI_DEBUG").is_some();
    let public = if let Some(fixture) = fixture {
        fs::read_to_string(append_extension(&fixture.key, ".pub"))?
    } else {
        env::var("MINARBOT_TAURI_UPDATER_PUBKEY")
            .map_err(|_| "Set MINARBOT_TAURI_UPDATER_PUBKEY.")?
    };
    let key = if let Some(fixture) = fixture {
        fs::read_to_string(&fixture.key)?
    } else {
        let value =
            env::var("TAURI_SIGNING_PRIVATE_KEY").map_err(|_| "Set TAURI_SIGNING_PRIVATE_KEY.")?;
        if Path::new(&value).is_file() {
            fs::read_to_string(&value)?
        } else {
            value
        }
    };
    let endpoint = fixture.map_or_else(
        || Ok::<_, url::ParseError>(format!("{GITHUB}/latest/download/{feed_name}")),
        |fixture| Ok(fixture.feed.join(&feed_name)?.into()),
    )?;
    PublicKey::decode(&decoded(&public)?)?;
    let mut runtime_file = fs::File::open(runtime)?;
    if !(1..=(1 << 30)).contains(&runtime_file.metadata()?.len()) {
        return Err("Invalid bundled runtime size.".into());
    }
    let mut hash = Sha256::new();
    io::copy(&mut (&mut runtime_file).take((1 << 30) + 1), &mut hash)?;
    let mut resources = serde_json::Map::new();
    resources.insert(
        runtime.to_str().ok_or("Invalid runtime path.")?.into(),
        json!("runtime.tar.gz"),
    );
    if cfg!(windows) {
        for script in ["install.sh", "install.ps1"] {
            let path = root.join("scripts").join(script);
            resources.insert(
                path.to_str().ok_or("Invalid installer path.")?.into(),
                json!(script),
            );
        }
    }
    let config = tempfile::Builder::new().suffix(".json").tempfile()?;
    serde_json::to_writer(
        config.as_file(),
        &json!({
            "productName": "Minarbot Tauri",
            "version": version,
            "bundle": {"resources": resources},
            "plugins": {"updater": {
                "pubkey": public.trim(),
                "endpoints": [endpoint],
                "requireSignedVersion": true,
                "dangerousInsecureTransportProtocol": fixture.is_some()
            }}
        }),
    )?;
    let mut build = cli(root);
    build
        .env("CARGO_TARGET_DIR", &target)
        .env(
            "MINARBOT_DESKTOP_RUNTIME_SHA256",
            format!("{:x}", hash.finalize()),
        )
        .env_remove("MINARBOT_DESKTOP_FIXTURE_PORT")
        .env_remove("TAURI_SIGNING_PRIVATE_KEY")
        .env_remove("TAURI_SIGNING_PRIVATE_KEY_PATH")
        .env_remove("TAURI_SIGNING_PRIVATE_KEY_PASSWORD")
        .args(["build", "--ci", "--bundles", bundle, "--config"])
        .arg(config.path());
    if let Some(fixture) = fixture {
        build.env("MINARBOT_DESKTOP_FIXTURE_PORT", fixture.port.to_string());
        if debug {
            build.arg("--debug");
        }
    }
    build.args(["--", "--locked"]);
    run(&mut build)?;
    let extension = artifact_name
        .rsplit_once('.')
        .ok_or("Missing bundle extension.")?
        .1;
    let bundled = find_bundle(
        &target
            .join(if debug { "debug" } else { "release" })
            .join("bundle")
            .join(bundle),
        extension,
        version,
    )?;
    let stage = tempfile::tempdir_in(output)?;
    let artifact = stage.path().join(&artifact_name);
    fs::copy(bundled, &artifact)?;
    let mut sign = cli(root);
    sign.env("TAURI_SIGNING_PRIVATE_KEY", key.trim())
        .env_remove("TAURI_SIGNING_PRIVATE_KEY_PATH")
        .args(["signer", "sign", "--app-version", version])
        .arg(&artifact);
    if fixture.is_some() {
        sign.env_remove("TAURI_SIGNING_PRIVATE_KEY_PASSWORD");
    }
    run(&mut sign)?;
    let signature_path = stage.path().join(&signature_name);
    let signature = fs::read_to_string(signature_path)?;
    assert_signature(&public, &artifact, &signature, version)?;
    let url = match fixture {
        None => format!("{GITHUB}/download/v{version}/{artifact_name}"),
        Some(fixture) => fixture.feed.join(&artifact_name)?.into(),
    };
    fs::write(
        stage.path().join(&feed_name),
        serde_json::to_vec_pretty(&json!({
            "version": version, "url": url, "signature": signature.trim(),
        }))?,
    )?;
    for name in [signature_name, artifact_name, feed_name] {
        fs::rename(stage.path().join(&name), output.join(name))?;
    }
    Ok(())
}

#[cfg(unix)]
pub fn verify_feed(directory: &Path, version: &str, platform: &str) -> Result<()> {
    let [signature, artifact, feed] = tauri_names(version, platform);
    let value: Value = serde_json::from_slice(&fs::read(directory.join(feed))?)?;
    if value["version"] != version
        || value["url"] != format!("{GITHUB}/download/v{version}/{artifact}")
        || value["signature"]
            .as_str()
            .is_none_or(|signature| signature.is_empty())
        || value["signature"] != fs::read_to_string(directory.join(signature))?.trim()
    {
        return Err(format!("Invalid Tauri update feed for {platform}.").into());
    }
    Ok(())
}

#[cfg(unix)]
pub fn release(
    root: &Path,
    runtime: &Path,
    output: &Path,
    version: &str,
    dry: bool,
    pilot: bool,
) -> Result<Vec<String>> {
    electron(root, runtime, output, version, dry)?;
    if !dry && !pilot {
        return Ok(Vec::new());
    }
    let temp = tempfile::tempdir()?;
    let fixture = if dry {
        let key = temp.path().join("fixture.key");
        run(cli(root)
            .args(["signer", "generate", "--ci", "--write-keys"])
            .arg(&key))?;
        Some(Fixture {
            key,
            feed: url::Url::parse(&format!("{GITHUB}/latest/download/"))?,
            port: 1,
        })
    } else {
        None
    };
    build_tauri(root, runtime, output, version, fixture.as_ref())?;
    Ok(tauri_names(version, platform()?).into())
}

pub fn command(args: &[String]) -> Result<()> {
    // Match existing release commands' explicit, fixed arguments.
    let mut args = args.iter();
    let mut options = std::collections::BTreeMap::new();
    let (mut legacy_only, mut tauri_only) = (false, false);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--legacy-only" => legacy_only = true,
            "--tauri-only" => tauri_only = true,
            "--runtime" | "--output" | "--version" | "--fixture-key" | "--fixture-feed"
            | "--fixture-port" => {
                let value = args
                    .next()
                    .filter(|value| !value.starts_with("--"))
                    .ok_or("Missing desktop option value.")?;
                if options.insert(flag.as_str(), value.as_str()).is_some() {
                    return Err("Duplicate desktop option.".into());
                }
            }
            _ => return Err(format!("Unknown desktop option {flag}.").into()),
        }
    }
    let get = |flag| {
        options
            .get(flag)
            .copied()
            .ok_or_else(|| format!("Pass {flag} to cargo xtask desktop."))
    };
    let root = env::current_dir()?;
    let runtime = fs::canonicalize(get("--runtime")?)?;
    let output = std::path::absolute(get("--output")?)?;
    fs::create_dir_all(&output)?;
    let app: Value = serde_json::from_slice(&fs::read(root.join("app/package.json"))?)?;
    let version = Version::parse(
        options
            .get("--version")
            .copied()
            .or_else(|| app["version"].as_str())
            .ok_or("Missing desktop version.")?,
    )?
    .to_string();
    let fixture = if options.keys().any(|flag| flag.starts_with("--fixture-")) {
        if !tauri_only || legacy_only {
            return Err("Fixture builds require --tauri-only.".into());
        }
        let feed = url::Url::parse(get("--fixture-feed")?)?;
        if feed.scheme() != "http"
            || !matches!(feed.host_str(), Some("127.0.0.1" | "[::1]"))
            || !feed.username().is_empty()
            || feed.password().is_some()
            || feed.query().is_some()
            || feed.fragment().is_some()
            || !feed.path().ends_with('/')
        {
            return Err("Fixture feed must be a loopback HTTP directory.".into());
        }
        let port: u16 = get("--fixture-port")?.parse()?;
        if port <= 1024 || port == 4317 {
            return Err("Choose a disposable high fixture port other than 4317.".into());
        }
        Some(Fixture {
            key: fs::canonicalize(get("--fixture-key")?)?,
            feed,
            port,
        })
    } else {
        None
    };
    if legacy_only && tauri_only {
        return Err("Choose at most one desktop lane.".into());
    }
    if !tauri_only {
        electron(&root, &runtime, &output, &version, false)?;
    }
    if !legacy_only {
        build_tauri(&root, &runtime, &output, &version, fixture.as_ref())?;
    }
    Ok(())
}
