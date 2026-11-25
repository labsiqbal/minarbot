//! Guide infrastructure setup (port of `app/guide-infra.test.ts`).

use minarbot_testkit::{
    base64::{
        engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
        Engine,
    },
    http::Method,
    model::{Reply, ScriptedModel},
    prelude::*,
    protocol::sha256_hex,
    server::Gateway,
};
use std::{
    collections::HashMap,
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{Arc, Mutex},
};

fn random_bytes(count: usize) -> Vec<u8> {
    let mut out = Vec::new();
    while out.len() < count {
        let id = new_id().replace('-', "");
        out.extend(
            (0..id.len())
                .step_by(2)
                .filter_map(|index| u8::from_str_radix(&id[index..index + 2], 16).ok()),
        );
    }
    out.truncate(count);
    out
}

fn hex_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .filter_map(|index| u8::from_str_radix(&hex[index..index + 2], 16).ok())
        .collect()
}

/// `/bsi_[A-Za-z0-9_-]{43}/`.
fn has_install_credential(text: &str) -> bool {
    text.match_indices("bsi_").any(|(index, _)| {
        text[index + 4..]
            .chars()
            .take(43)
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .count()
            == 43
    })
}

fn script(path: &Path, lines: &[String]) -> Result<()> {
    std::fs::write(path, lines.join("\n"))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

struct Guide<'a> {
    app: &'a Server,
    origin: String,
    observations: &'a Mutex<Vec<String>>,
    turns: String,
}

impl Guide<'_> {
    async fn api(&self, path: &str, body: Option<Value>, status: u16) -> Result<Value> {
        let method = if body.is_some() {
            Method::POST
        } else {
            Method::GET
        };
        let mut request = self
            .app
            .client()?
            .request(method, self.app.url(path))
            .header("Origin", &self.origin)
            .header("Content-Type", "application/json")
            .header("Sec-Fetch-Site", "same-origin");
        if let Some(body) = body {
            request = request.body(body.to_string());
        }
        let response = request.send().await?;
        let code = response.status().as_u16();
        let raw = response.text().await?;
        if let Ok(mut observations) = self.observations.lock() {
            observations.push(raw.clone());
        }
        ensure!(code == status, "{path}: HTTP {code} {raw}");
        Ok(serde_json::from_str(&raw)?)
    }

    async fn send(&self, text: &str) -> Result<Value> {
        let id = new_id();
        self.api(&self.turns, Some(json!({"id": id, "text": text})), 202)
            .await?;
        let turn = self
            .app
            .wait_row(&self.turns, &id, |row| row["status"] == "done")
            .await?;
        if let Ok(mut observations) = self.observations.lock() {
            observations.push(turn.to_string());
        }
        Ok(turn["guide"]["changes"][0].clone())
    }

    async fn apply(&self, proposal: &Value, status: u16, action: &str) -> Result<Value> {
        let id = proposal["id"].as_str().ok_or("proposal id")?;
        self.api(
            &format!("/api/guide/changes/{id}"),
            Some(json!({"action": action, "change": proposal["change"]})),
            status,
        )
        .await
    }
}

#[tokio::test]
async fn guide_infrastructure_requires_approval_and_keeps_credentials_out_of_replies_storage_and_logs(
) -> Result<()> {
    let source = "https://source.example";
    let tail = "https://node.fixture.ts.net";
    let target = "https://vps.example";
    let password = URL_SAFE_NO_PAD.encode(random_bytes(24));
    let browser_token = format!("bsi_{}", URL_SAFE_NO_PAD.encode(random_bytes(32)));
    let digest = URL_SAFE_NO_PAD.encode(hex_bytes(&sha256_hex(&browser_token)));
    let mut key = hex_bytes("0000000b7373682d6564323535313900000020");
    key.extend(random_bytes(32));
    let public_key = STANDARD.encode(key);
    let plans: HashMap<&str, (&str, Value)> = HashMap::from([
        (
            "machine",
            (
                "add_guide_machine",
                json!({"name": "Test machine", "origin": target}),
            ),
        ),
        (
            "vps",
            (
                "install_guide_vps",
                json!({"host": "vps.example", "user": "install", "origin": target,
                    "hostKey": format!("ssh-ed25519 {public_key}"), "vaultEntry": "fixture-vps"}),
            ),
        ),
        (
            "remote",
            ("configure_guide_remote_access", json!({"origin": tail})),
        ),
        ("detect", ("inspect_guide_remote_access", json!({}))),
        ("computer", ("setup_guide_computer", json!({}))),
    ]);
    let model = ScriptedModel::start().await?;
    model.respond(move |request| {
        let prompt = request.last_user_text();
        let last_is_tool = request
            .messages()
            .last()
            .is_some_and(|message| message["role"] == "tool");
        match plans.get(prompt.as_str()) {
            Some((tool, args)) if !last_is_tool => {
                let mut args = args.clone();
                if prompt != "detect" {
                    args["requestId"] = json!(new_id());
                }
                Reply::tool(tool, args)
            }
            _ => Reply::text("Review the setup card."),
        }
    });
    let root = TestRoot::new()?;
    let bin = root.dir("bin")?;
    let releases = root.dir("releases/0.17.0-8523c26b")?;
    let releases = releases.parent().ok_or("releases")?.to_owned();
    std::os::unix::fs::symlink("0.17.0-8523c26b", releases.join("current"))?;
    let env_file = root.path().join("launcher.env");
    std::fs::write(
        &env_file,
        format!("MINARBOT_ALLOWED_ORIGINS={source}\nUNRELATED=\"first\nsecond\"\n"),
    )?;
    std::fs::set_permissions(&env_file, std::fs::Permissions::from_mode(0o600))?;
    let trace = root.path().join("ssh-trace");
    let password_hash = sha256_hex(&password);
    let trace_text = trace.to_string_lossy();
    script(
        &bin.join("ssh"),
        &[
            "#!/bin/sh".into(),
            "set -eu".into(),
            "pass=$(\"$SSH_ASKPASS\" \"fixture password\")".into(),
            "hash=$(printf \"%s\" \"$pass\" | sha256sum)".into(),
            format!("[ \"$hash\" = \"{password_hash}  -\" ]"),
            "printf \"%s\" \"$pass\"; printf \"%s\" \"$pass\" >&2".into(),
            "for command do :; done".into(),
            "case \"$command\" in".into(),
            format!("  *rollback) printf \"rollback\\n\" >> \"{trace_text}\"; exit 0;;"),
            "esac".into(),
            "token=$(cat)".into(),
            "case \"$token\" in bsi_*) ;; *) exit 2;; esac".into(),
            "for argument do case \"$argument\" in *\"$pass\"*|*\"$token\"*) exit 3;; esac; done"
                .into(),
            "printf \"%s\" \"$token\"; printf \"%s\" \"$token\" >&2".into(),
            format!("printf \"install\\n\" >> \"{trace_text}\""),
            "exit 1".into(),
            String::new(),
        ],
    )?;
    let config = ServerConfig::new()
        .gateway(Gateway {
            api_key: Some("guide-provider-key".into()),
            default_model: Some("fixture".into()),
            ..Gateway::new(model.base_url())
        })
        .option("testKeyring", json!({}))
        .path_prefix(&bin)
        .option("allowedOrigins", json!([source]))
        .option("updates", json!({"releasesDir": releases}))
        .env("MINARBOT_LAUNCHER_ENV_FILE", env_file.to_string_lossy())
        // The SSH askpass socket lives in `$TMPDIR/minarbot-ssh-<id>/`; the
        // kit's per-test TMPDIR makes that path exceed the 108-byte Unix
        // socket limit, so this server uses the system temp directory like
        // the Node test did.
        .env("TMPDIR", std::env::temp_dir().to_string_lossy());
    let app = root.start(config).await?;
    let local = app.origin().to_owned();
    let status = json!({"BackendState": "Running", "CurrentTailnet": {"MagicDNSEnabled": true},
        "Self": {"DNSName": "node.fixture.ts.net.", "UserID": "fixture-private-peer-metadata"},
        "CertDomains": ["node.fixture.ts.net"]});
    let serve = json!({"TCP": {"443": {"HTTPS": true}},
        "Web": {"node.fixture.ts.net:443": {"Handlers": {"/": {"Proxy": local}}}}});
    script(
        &bin.join("tailscale"),
        &[
            "#!/bin/sh".into(),
            "case \"$*\" in".into(),
            format!("  'status --json') printf '%s\\n' '{status}';;"),
            format!("  'serve status --json') printf '%s\\n' '{serve}';;"),
            "  *) exit 9;;".into(),
            "esac".into(),
            String::new(),
        ],
    )?;
    let observations: Arc<Mutex<Vec<String>>> = Arc::default();
    let mut guide = Guide {
        app: &app,
        origin: local.clone(),
        observations: &observations,
        turns: String::new(),
    };
    let agents = guide.api("/api/agents", None, 200).await?;
    let guide_agent = agents["guide"].clone();
    let guide_id = guide_agent["id"].as_str().ok_or("guide id")?.to_owned();
    // #87 removed `providerAccount`; the test-mode runtime shortcut binds the gateway.
    guide
        .api(
            &format!("/api/agents/{guide_id}/edit"),
            Some(
                json!({"id": new_id(), "revision": guide_agent["revision"], "model": "fixture",
                "runtime": "api", "confirmRuntime": true}),
            ),
            200,
        )
        .await?;
    guide.turns = format!("/api/agents/{guide_id}/turns");
    guide
        .api(
            "/api/vault",
            Some(
                json!({"name": "fixture-vps", "kind": "secret", "revision": 0, "url": target,
                "value": password, "agents": [guide_id]}),
            ),
            200,
        )
        .await?;

    // Adding a machine needs approval of the exact proposal, once.
    let machine = guide.send("machine").await?;
    let tokens = json!({"id": machine["id"], "digest": digest});
    guide
        .api("/api/install/tokens", Some(tokens.clone()), 409)
        .await?;
    let mut altered = machine.clone();
    altered["change"]["target"] = json!({"origin": source});
    guide.apply(&altered, 409, "apply").await?;
    ensure!(
        guide.apply(&machine, 200, "apply").await?["installRequest"] == machine["id"],
        "install request"
    );
    let receipt = guide
        .api("/api/install/tokens", Some(tokens.clone()), 201)
        .await?;
    ensure!(receipt["source"] == source, "receipt: {receipt}");
    ensure!(
        receipt.get("token").is_none(),
        "receipt returns the token: {receipt}"
    );
    guide.api("/api/install/tokens", Some(tokens), 409).await?;
    guide.apply(&machine, 409, "apply").await?;
    let invite = app
        .client()?
        .post(app.url("/api/install/pairing-code"))
        .bearer_auth(&browser_token)
        .send()
        .await?;
    ensure!(
        invite.status().as_u16() == 200,
        "pairing code: {}",
        invite.status()
    );
    let code = invite.json::<Value>().await?["code"]
        .as_str()
        .ok_or("pairing code")?
        .to_owned();
    let denied = app
        .client()?
        .post(app.url("/api/pairing/redeem"))
        .header("Content-Type", "application/json")
        .body(
            json!({"code": code, "returnPairing": {"id": new_id(), "name": "Wrong target",
                "version": "0.17.0", "origin": "https://other.example"}})
            .to_string(),
        )
        .send()
        .await?;
    ensure!(
        denied.status().as_u16() == 400,
        "Pairing code must match the approved origin: {}",
        denied.status()
    );

    // Remote access changes the launcher file only on approval.
    let remote = guide.send("remote").await?;
    ensure!(
        !std::fs::read_to_string(&env_file)?.contains(tail),
        "remote access applied before approval"
    );
    guide.send("detect").await?;
    let mut observed = observations
        .lock()
        .map(|seen| seen.clone())
        .unwrap_or_default();
    observed.extend(
        model
            .requests()
            .iter()
            .map(|request| request.body.to_string()),
    );
    let detected = observed.iter().any(|value| {
        value.contains("reachableFromServer") && value.contains("node.fixture.ts.net")
    });
    ensure!(detected, "remote access inspection not observed");
    guide.apply(&remote, 200, "apply").await?;
    let updated = std::fs::read_to_string(&env_file)?;
    ensure!(
        updated.contains(&format!("MINARBOT_ALLOWED_ORIGINS={source},{tail}")),
        "launcher env: {updated}"
    );
    ensure!(
        updated.contains("UNRELATED=\"first\nsecond\""),
        "launcher env: {updated}"
    );
    guide
        .api(
            "/api/device-pairing/code",
            Some(json!({"origin": tail})),
            403,
        )
        .await?;
    let qr = guide
        .api(
            "/api/device-pairing/code",
            Some(json!({"origin": source})),
            200,
        )
        .await?;
    ensure!(qr["url"] == format!("{source}/pair"), "qr: {qr}");
    ensure!(
        qr["qr"]
            .as_str()
            .is_some_and(|value| value.starts_with("data:image/svg+xml;base64,")),
        "qr image: {}",
        qr["qr"]
    );

    // A VPS install runs only after approval and rolls back on failure.
    let vps = guide.send("vps").await?;
    ensure!(!trace.exists(), "ssh ran before approval");
    guide.apply(&vps, 200, "cancel").await?;
    guide.apply(&vps, 409, "apply").await?;
    ensure!(!trace.exists(), "cancelled install ran");
    let second = guide.send("vps").await?;
    let failed = guide.apply(&second, 409, "apply").await?;
    ensure!(
        failed["error"]
            .as_str()
            .is_some_and(|error| error.contains("stopped")),
        "failed install: {failed}"
    );
    ensure!(
        std::fs::read_to_string(&trace)? == "install\nrollback\n",
        "ssh trace: {}",
        std::fs::read_to_string(&trace)?
    );

    let computer = guide
        .apply(&guide.send("computer").await?, 200, "apply")
        .await?;
    ensure!(
        computer["setup"]["state"].is_string(),
        "computer: {computer}"
    );
    let declared: Vec<String> = model
        .requests()
        .iter()
        .flat_map(|request| request.tool_names())
        .collect();
    ensure!(
        declared.iter().any(|name| name == "install_guide_vps")
            && declared
                .iter()
                .any(|name| name == "inspect_guide_remote_access")
            && !declared.iter().any(|name| name == "run_shell"),
        "declared tools: {declared:?}"
    );

    // No credential appears in replies, provider requests, logs or storage.
    let (stdout, stderr) = app.output()?;
    let mut seen = observations
        .lock()
        .map(|seen| seen.clone())
        .unwrap_or_default();
    seen.extend(
        model
            .requests()
            .iter()
            .map(|request| request.body.to_string()),
    );
    seen.push(stdout);
    seen.push(stderr);
    for raw in &seen {
        ensure!(!raw.contains(&password), "SSH password leaked");
        ensure!(!raw.contains(&browser_token), "Install credential leaked");
        ensure!(!raw.contains(&code), "Machine pairing code leaked");
        ensure!(
            !raw.contains("fixture-private-peer-metadata"),
            "Tailscale private status leaked"
        );
        ensure!(
            !has_install_credential(raw),
            "Server install credential leaked"
        );
    }
    let data = app.database().parent().ok_or("data dir")?.to_owned();
    for entry in std::fs::read_dir(&data)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.file_type()?.is_file() || !name.starts_with("state.sqlite") {
            continue;
        }
        let bytes = std::fs::read(entry.path())?;
        let contains = |needle: &str| {
            bytes
                .windows(needle.len())
                .any(|window| window == needle.as_bytes())
        };
        ensure!(
            !contains(&password) && !contains(&browser_token),
            "Credential persisted in plaintext in {name}"
        );
        let latin: String = bytes.iter().map(|byte| char::from(*byte)).collect();
        ensure!(
            !has_install_credential(&latin),
            "Server install credential persisted in plaintext in {name}"
        );
    }
    Ok(())
}
