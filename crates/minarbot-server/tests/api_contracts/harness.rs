use super::json;
use crate::server::{Result, TestRoot};
use serde_json::json;
use std::{ffi::OsStr, fs, os::unix::fs::PermissionsExt, path::Path};

fn executable(path: &Path, script: &str) -> Result<()> {
    fs::write(path, script)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[tokio::test]
async fn device_sign_in_runs_mise_without_resolving_its_own_shim() -> Result<()> {
    let root = TestRoot::new()?;
    let dir = root.path();
    let owner = dir.join("owner");
    let xdg_data = dir.join("xdg-data");
    let mise_data = xdg_data.join("mise");
    let shims = mise_data.join("shims");
    fs::create_dir_all(owner.join(".claude"))?;
    fs::create_dir_all(&shims)?;
    let credentials = owner.join(".claude/.credentials.json");
    fs::write(&credentials, "{}")?;
    fs::set_permissions(credentials, fs::Permissions::from_mode(0o600))?;
    let shim = dir.join("claude");
    executable(
        &shim,
        "#!/bin/sh\nmise use -g --quiet claude || exit 1\nexec mise x claude -- claude \"$@\"\n",
    )?;
    executable(
        &dir.join("mise"),
        r#"#!/bin/sh
case ":$PATH:" in *":$MISE_DATA_DIR/"*) exit 3;; esac
case "$1" in
  use) exit 0;;
  x) shift 4
    [ "$1" = --version ] && { echo 'claude fixture 1.0'; exit 0; }
    [ "$1 $2" = 'auth status' ] && [ -f "$HOME/.claude/.credentials.json" ] && exit 0
    exit 1;;
esac
exit 2
"#,
    )?;
    let shell = dir.join("fixture-shell");
    executable(&shell, "#!/bin/sh\nexec /bin/sh -c \"$2\"\n")?;
    let path = std::env::join_paths([dir, &shims, Path::new("/usr/bin"), Path::new("/bin")])?;
    let environment: [(&str, &OsStr); 5] = [
        ("PATH", &path),
        ("HOME", owner.as_os_str()),
        ("SHELL", shell.as_os_str()),
        ("MISE_DATA_DIR", mise_data.as_os_str()),
        ("XDG_DATA_HOME", xdg_data.as_os_str()),
    ];
    let app = root
        .start_with_env(
            json!({"harness":{"executables":{"claude":shim}}}),
            &environment,
        )
        .await?;
    let runtimes = json(app.get("/api/runtimes"), 200).await?;
    let claude = runtimes["choices"]
        .as_array()
        .ok_or("missing runtime choices")?
        .iter()
        .find(|row| row["runtime"] == "claude")
        .ok_or("Claude absent from runtime catalog")?;
    assert_eq!(
        [
            &claude["available"],
            &claude["version"],
            &claude["signedIn"]
        ],
        [&json!(true), &json!("claude fixture 1.0"), &json!(true)],
        "{:?}",
        claude["reason"]
    );
    Ok(())
}
