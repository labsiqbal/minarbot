mod gates;
mod github;
mod release;
mod served;
mod smoke;

use minarbot_launcher::files;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    env,
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};
use tempfile::Builder;

type Result<T> = minarbot_launcher::Result<T>;
pub const REPOSITORY: &str = "labsiqbal/minarbot";

fn output(command: &mut Command) -> Result<String> {
    let output = command.env_remove("MINARBOT_GHCR_TOKEN").output()?;
    if !output.status.success() {
        return Err(format!(
            "{:?} failed ({}): {}",
            command.get_program(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn run(command: &mut Command) -> Result<()> {
    let status = command.env_remove("MINARBOT_GHCR_TOKEN").status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Command failed: {:?} ({status})", command).into())
    }
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    Ok(output(Command::new("git").current_dir(root).args(args))?
        .trim()
        .to_owned())
}

fn check_clean(root: &Path) -> Result<()> {
    if !git(root, &["status", "--porcelain"])?.is_empty() {
        return Err(
            "Refusing dirty worktree. Use --allow-dirty for a local development release.".into(),
        );
    }
    Ok(())
}

fn check_head(root: &Path, commit: &str) -> Result<()> {
    check_clean(root)?;
    if git(root, &["rev-parse", "HEAD"])? != commit {
        return Err("Worktree HEAD changed during the release build.".into());
    }
    Ok(())
}

fn sha256(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn durable(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = path.parent().ok_or("Missing output directory")?;
    let mut stage = tempfile::NamedTempFile::new_in(parent)?;
    stage.write_all(bytes)?;
    stage
        .as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    stage.as_file().sync_all()?;
    stage.persist(path).map_err(|error| error.error)?;
    files::sync_dir(parent)
}

fn private_stage(prefix: &str, directory: &Path) -> Result<tempfile::TempDir> {
    Ok(Builder::new()
        .prefix(prefix)
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(directory)?)
}

fn scratch(prefix: &str) -> Result<tempfile::TempDir> {
    private_stage(prefix, &env::temp_dir())
}

fn copy(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.is_dir() {
        fs::create_dir_all(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy(&entry.path(), &destination.join(entry.file_name()))?;
        }
    } else if metadata.is_file() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, destination)?;
    } else {
        return Err(format!("Release contains unsupported file: {}", source.display()).into());
    }
    Ok(())
}

fn archive(source: &Path, path: &Path) -> Result<()> {
    run(Command::new("tar")
        .args(["--format=ustar", "-czf"])
        .arg(path)
        .arg("-C")
        .arg(source)
        .arg(".")
        .env("COPYFILE_DISABLE", "1"))
}

pub(crate) fn options(
    args: &[String],
    switches: &[&str],
    values: &[&str],
) -> Result<BTreeMap<String, String>> {
    let mut options = BTreeMap::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" && args.as_slice().is_empty() {
            break;
        }
        let (flag, inline) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(key, value)| (key, Some(value)));
        let value = if switches.contains(&flag) && inline.is_none() {
            String::new()
        } else if values.contains(&flag) {
            inline
                .map(str::to_owned)
                .or_else(|| args.next().filter(|arg| !arg.starts_with("--")).cloned())
                .ok_or_else(|| format!("Missing value for {flag}"))?
        } else {
            return Err(format!("Unknown option: {arg}").into());
        };
        options.insert(flag.into(), value);
    }
    Ok(options)
}

fn execute(args: &[String], public_key: &[u8]) -> Result<()> {
    let root = env::current_dir()?;
    if let Some((command, args)) = args.split_first() {
        match command.as_str() {
            "release" => return release::command(&root, args, public_key),
            "resource-gate" => return gates::resource_command(args),
            "production-env-gate" => return gates::production_command(&root, args),
            "install-smoke" => return smoke::command(&root, args),
            "check-served" => return served::command(args),
            "test-port" if args.is_empty() => {
                println!("{}", gates::free_port()?);
                return Ok(());
            }
            "stage-runtime" if args.len() == 2 => {
                return release::stage_runtime(Path::new(&args[0]), Path::new(&args[1]))
            }
            "native-dependencies" if args.len() == 1 => {
                return release::native_dependencies(Path::new(&args[0]))
            }
            "help" | "--help" | "-h" => {}
            _ => return Err(format!("Unknown xtask command or arguments: {command}").into()),
        }
    }
    println!("cargo xtask <release|check-served|install-smoke|resource-gate|production-env-gate|test-port>");
    Ok(())
}

fn main() {
    if let Err(error) = execute(
        &env::args().skip(1).collect::<Vec<_>>(),
        &minarbot_launcher::RELEASE_PUBLIC_KEY,
    ) {
        eprintln!("Error: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
#[test]
#[ignore = "Invoked by app release fixtures"]
fn app_fixture() -> Result<()> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde_json::Value;

    let root = env::current_dir()?;
    let options: Value = serde_json::from_str(&env::var("MINARBOT_XTASK_FIXTURE")?)?;
    let args: Vec<String> = serde_json::from_value(options["args"].clone())?;
    let reply = options["reply"].as_str().map(Path::new);
    let result = (|| -> Result<()> {
        let value = match args.first().map(String::as_str) {
            Some("build") => {
                serde_json::to_value(release::fixture_build(&root, options["build"].clone())?)?
            }
            Some("parse") => {
                serde_json::to_value(release::fixture_parse(options["value"].clone())?)?
            }
            Some("manifest") => serde_json::to_value(files::manifest(Path::new(&args[1]))?)?,
            Some("verify") => {
                serde_json::to_value(release::verify(Path::new(&args[1]), &args[2])?)?
            }
            Some("select") => {
                files::select(Path::new(&args[1]), &args[2], &args[3])?;
                Value::Null
            }
            Some("selection") => {
                serde_json::to_value(files::selection(Path::new(&args[1]), &args[2])?)?
            }
            _ => {
                let key = options["publicKey"]
                    .as_str()
                    .map(|key| STANDARD.decode(key))
                    .transpose()?;
                execute(
                    &args,
                    key.as_deref()
                        .unwrap_or(&minarbot_launcher::RELEASE_PUBLIC_KEY),
                )?;
                Value::Null
            }
        };
        if let Some(reply) = reply {
            fs::write(reply, serde_json::to_vec(&value)?)?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("Error: {error}");
        std::process::exit(1);
    }
    Ok(())
}
