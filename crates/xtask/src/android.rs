//! `cargo xtask android`: build the Minarbot phone app (`mobile/tauri`) as a
//! debug APK on this host. See mobile/README.md for the SDK it expects.

use super::*;

const USAGE: &str = "cargo xtask android [--target aarch64|armv7|i686|x86_64]";

pub fn command(root: &Path, args: &[String]) -> Result<()> {
    let options =
        options(args, &[], &["--target"]).map_err(|error| format!("{error}. Usage: {USAGE}"))?;
    let target = options.get("--target").map_or("aarch64", String::as_str);
    if !matches!(target, "aarch64" | "armv7" | "i686" | "x86_64") {
        return Err(format!("Unknown Android target {target}. Usage: {USAGE}").into());
    }
    let home = PathBuf::from(env::var_os("HOME").ok_or("HOME is not set")?);
    let sdk = env::var_os("ANDROID_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("Android/Sdk"));
    let ndk = match env::var_os("NDK_HOME") {
        Some(ndk) => PathBuf::from(ndk),
        None => newest_ndk(&sdk)?,
    };
    let cli = desktop::install_cli(root)?;
    // Gradle's Rust step runs `cargo tauri`, so the pinned CLI goes first on PATH.
    let mut path = vec![cli.parent().ok_or("tauri-cli has no directory")?.to_owned()];
    path.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));
    run(Command::new(&cli)
        .current_dir(root.join("mobile/tauri"))
        .env("PATH", env::join_paths(path)?)
        .env("ANDROID_HOME", &sdk)
        .env("NDK_HOME", &ndk)
        .args([
            "android", "build", "--debug", "--apk", "--ci", "--target", target,
        ]))?;
    let apk = root.join(
        "mobile/tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk",
    );
    if !apk.is_file() {
        return Err(format!("The build left no APK at {}.", apk.display()).into());
    }
    println!("APK: {}", apk.display());
    println!("Install: adb install -r {}", apk.display());
    Ok(())
}

/// The newest NDK under the SDK, by version-numbered directory name.
fn newest_ndk(sdk: &Path) -> Result<PathBuf> {
    let directory = sdk.join("ndk");
    let mut versions = fs::read_dir(&directory)
        .map_err(|error| format!("No NDK in {} ({error}). Set NDK_HOME.", directory.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.join("source.properties").is_file())
        .collect::<Vec<_>>();
    versions.sort_by_key(|path| ndk_version(path));
    versions
        .pop()
        .ok_or_else(|| format!("No NDK in {}. Set NDK_HOME.", directory.display()).into())
}

fn ndk_version(path: &Path) -> Vec<u64> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| {
            name.split('.')
                .filter_map(|part| part.parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_newest_installed_ndk() -> Result<()> {
        let sdk = tempfile::tempdir()?;
        for version in ["9.1.0", "28.2.13676358", "27.0.12077973", "30.0.0-broken"] {
            let directory = sdk.path().join("ndk").join(version);
            fs::create_dir_all(&directory)?;
            if version != "30.0.0-broken" {
                fs::write(directory.join("source.properties"), "")?;
            }
        }
        assert_eq!(
            newest_ndk(sdk.path())?,
            sdk.path().join("ndk/28.2.13676358")
        );
        assert!(newest_ndk(&sdk.path().join("missing")).is_err());
        Ok(())
    }
}
