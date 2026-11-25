//! Signed infrastructure fence for canonical restore activation (port of
//! `app/restore-fence.test.ts`). Each test drives the real
//! `minarbot-server restore ...` operator commands.

use minarbot_testkit::{
    canonical::Canonical,
    db::Fixture,
    prelude::*,
    protocol::sha256_hex,
    restore::{self, FenceKey, Identity},
};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

const ORIGIN: &str = "https://minarbot.example";
const PROJECT: &str = "22222222-2222-4222-8222-222222222222";
const OTHER_ID: &str = "33333333-3333-4333-8333-333333333333";

/// Outcome of one restore command: the JSON without `exitCode`/`database`
/// on success, the full JSON on failure.
type Outcome = std::result::Result<Value, Value>;

fn with(base: &Value, fields: Value) -> Value {
    let mut value = base.clone();
    if let (Some(value), Some(fields)) = (value.as_object_mut(), fields.as_object()) {
        value.extend(fields.clone());
    }
    value
}

/// Write `value` as JSON; a new file is created private (0600), an existing
/// file keeps its mode, as Node's `writeFileSync(..., {mode})` did.
fn write(path: &Path, value: &Value) -> Result<()> {
    write_text(path, &value.to_string())
}

fn write_text(path: &Path, text: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(text.as_bytes())?;
    Ok(())
}

fn read_json(path: &str) -> Result<Value> {
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

fn evidence_body(evidence: &Value) -> Value {
    json!([
        "minarbot-infrastructure-fence-v1",
        evidence["request"],
        evidence["fenceId"],
        evidence["processFenced"],
        evidence["routeFenced"],
        evidence["admissionsFenced"],
        evidence["observedAt"],
        evidence["expiresAt"]
    ])
}

struct Fence {
    _canonical: Canonical,
    dir: PathBuf,
    database: PathBuf,
    owner: Identity,
    owner_account: String,
    proof: Value,
    trust: Value,
    key: FenceKey,
    trust_file: PathBuf,
    evidence_file: PathBuf,
    plan_file: PathBuf,
    alternate_session: PathBuf,
    session_file: String,
    db: Option<Fixture>,
}

impl Fence {
    async fn new(seed: impl FnOnce(&Fixture, &str) -> Result<()>) -> Result<Self> {
        let canonical = Canonical::new()?;
        let dir = canonical.root.dir("fence")?;
        let database = dir.join("restored.sqlite");
        let owner_account = canonical.owner.account_id.clone();
        canonical.db.execute(
            "INSERT INTO projects(id,account,name,private,deleted,revision) VALUES (?,?,?,1,0,1)",
            (PROJECT, &owner_account, "Restore"),
        )?;
        canonical.db.execute(
            "INSERT INTO project_members(project,account,role,grant_revision,deleted) VALUES (?,?,'owner',1,0)",
            (PROJECT, &owner_account),
        )?;
        seed(&canonical.db, &owner_account)?;
        let snapshot = snapshot(&canonical, &dir)?;
        let read_only = restore::run(
            &dir,
            &[
                "--read-only",
                &snapshot.to_string_lossy(),
                "--db",
                &database.to_string_lossy(),
            ],
            &[],
            None,
        )?;
        fs::remove_dir_all(snapshot.parent().ok_or("snapshot directory")?)?;
        ensure!(read_only["exitCode"] == 0, "read-only restore: {read_only}");
        let owner = Identity::new("user_owner", ORIGIN);
        let key = FenceKey::generate()?;
        let trust = json!({"v": 1, "endpoint": ORIGIN, "controllerId": "fixture-controller",
            "targetId": "restored-target", "signingPublicKey": key.public_spki()});
        let mut fence = Self {
            _canonical: canonical,
            trust_file: dir.join("trust.json"),
            evidence_file: dir.join("evidence.json"),
            plan_file: dir.join("plan.json"),
            alternate_session: dir.join("alternate-session.json"),
            dir,
            database,
            owner,
            owner_account,
            proof: Value::Null,
            trust,
            key,
            session_file: String::new(),
            db: None,
        };
        fence.session_file = fence.reconcile(
            read_only["bootstrapFile"]
                .as_str()
                .ok_or("bootstrap file")?,
        )?;
        fence.proof = read_json(&fence.session_file)?;
        fence.db = Some(Fixture::open(&fence.database)?);
        Ok(fence)
    }

    fn db(&self) -> Result<&Fixture> {
        self.db.as_ref().ok_or_else(|| "database closed".into())
    }

    fn reopen(&mut self) -> Result<()> {
        self.db = None;
        self.db = Some(Fixture::open(&self.database)?);
        Ok(())
    }

    fn db_path(&self) -> String {
        self.database.to_string_lossy().into_owned()
    }

    fn env(&self, actor: &Identity) -> Vec<(String, String)> {
        let mut env = actor.env();
        env.push((
            "MINARBOT_RESTORE_TRUST".into(),
            self.trust_file.to_string_lossy().into_owned(),
        ));
        env
    }

    /// Raw restore command as `actor` (exit code in the JSON).
    fn run(&self, args: &[&str], actor: &Identity) -> Result<Value> {
        restore::run(&self.dir, args, &self.env(actor), Some(&actor.input()?))
    }

    fn reconcile(&self, bootstrap: &str) -> Result<String> {
        let db = self.db_path();
        let value = self.run(
            &["reconcile", "--db", &db, "--bootstrap", bootstrap],
            &self.owner,
        )?;
        ensure!(value["exitCode"] == 0, "reconcile: {value}");
        Ok(value["sessionFile"]
            .as_str()
            .ok_or("session file")?
            .to_owned())
    }

    fn command(
        &self,
        name: &str,
        pin: &Value,
        actor: &Identity,
        gate: &str,
        paths: &[&str],
    ) -> Result<Outcome> {
        write(&self.trust_file, pin)?;
        let db = self.db_path();
        let mut args = vec![name, "--db", db.as_str(), "--session", gate];
        args.extend_from_slice(paths);
        let mut value = self.run(&args, actor)?;
        if value["exitCode"] != 0 {
            return Ok(Err(value));
        }
        if let Some(fields) = value.as_object_mut() {
            fields.remove("exitCode");
            fields.remove("database");
        }
        Ok(Ok(value))
    }

    fn prepare(&self) -> Result<Outcome> {
        Ok(
            match self.command(
                "fence-request",
                &self.trust,
                &self.owner,
                &self.session_file,
                &[],
            )? {
                Ok(value) => Ok(read_json(
                    value["requestFile"].as_str().ok_or("request file")?,
                )?),
                Err(error) => Err(error),
            },
        )
    }

    fn request(&self) -> Result<Value> {
        self.prepare()?
            .map_err(|error| format!("fence-request failed: {error}").into())
    }

    fn signed(&self, request: &Value, overrides: Value) -> Value {
        let now = restore::now();
        let mut evidence = with(
            &json!({"request": request, "fenceId": "fixture-fence-1", "processFenced": true,
                "routeFenced": true, "admissionsFenced": true, "observedAt": now,
                "expiresAt": now + 30_000}),
            overrides,
        );
        evidence["signature"] = json!(self.key.sign(&evidence_body(&evidence)));
        evidence
    }

    fn verify_as(
        &self,
        input: &Value,
        pin: &Value,
        actor: &Identity,
        gate: &str,
    ) -> Result<Outcome> {
        write(&self.evidence_file, input)?;
        let evidence = self.evidence_file.to_string_lossy().into_owned();
        self.command("verify-fence", pin, actor, gate, &["--evidence", &evidence])
    }

    fn verify(&self, input: &Value) -> Result<Outcome> {
        self.verify_as(input, &self.trust, &self.owner, &self.session_file)
    }

    fn plan(&self, input: &Value) -> Result<Outcome> {
        write(&self.evidence_file, input)?;
        let evidence = self.evidence_file.to_string_lossy().into_owned();
        Ok(
            match self.command(
                "activation-plan",
                &self.trust,
                &self.owner,
                &self.session_file,
                &["--evidence", &evidence],
            )? {
                Ok(value) => Ok(read_json(value["planFile"].as_str().ok_or("plan file")?)?),
                Err(error) => Err(error),
            },
        )
    }

    fn activate_as(
        &self,
        input: &Value,
        plan: &Value,
        pin: &Value,
        actor: &Identity,
        proof: &Value,
    ) -> Result<Outcome> {
        write(&self.evidence_file, input)?;
        write(&self.plan_file, plan)?;
        write(&self.alternate_session, proof)?;
        let (evidence, plan_file, gate) = (
            self.evidence_file.to_string_lossy().into_owned(),
            self.plan_file.to_string_lossy().into_owned(),
            self.alternate_session.to_string_lossy().into_owned(),
        );
        self.command(
            "activate",
            pin,
            actor,
            &gate,
            &["--evidence", &evidence, "--plan", &plan_file],
        )
    }

    fn activate(&self, input: &Value, plan: &Value) -> Result<Outcome> {
        self.activate_as(input, plan, &self.trust, &self.owner, &self.proof)
    }

    fn replace_session(&self) -> Result<Value> {
        let db = self.db_path();
        let gate = restore::run(&self.dir, &["bootstrap", "--db", &db], &[], None)?;
        ensure!(gate["exitCode"] == 0, "bootstrap: {gate}");
        read_json(&self.reconcile(gate["bootstrapFile"].as_str().ok_or("bootstrap file")?)?)
    }

    fn serving_state(&self) -> Result<String> {
        self.db()?
            .scalar("SELECT serving_state FROM authority_state", [])
    }
}

/// `workspaceSnapshot`: a private copy with the backup markers.
fn snapshot(canonical: &Canonical, dir: &Path) -> Result<PathBuf> {
    let directory = dir.join("backup");
    fs::create_dir(&directory)?;
    let file = directory.join("minarbot.sqlite");
    canonical
        .db
        .execute("VACUUM INTO ?", [file.to_string_lossy().as_ref()])?;
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
    let copy = Fixture::open(&file)?;
    copy.execute_batch(
        "CREATE TABLE IF NOT EXISTS minarbot_backup (format TEXT NOT NULL,version INTEGER NOT NULL,created TEXT NOT NULL) STRICT;
         DELETE FROM minarbot_backup;
         INSERT INTO minarbot_backup VALUES ('minarbot-canonical-v6',6,'2026-11-25T00:00:00.000Z');
         CREATE TABLE IF NOT EXISTS minarbot_backup_authority(generation TEXT NOT NULL,owner_account TEXT REFERENCES accounts(id)) STRICT;
         DELETE FROM minarbot_backup_authority;
         INSERT INTO minarbot_backup_authority SELECT generation,(SELECT DISTINCT account FROM projects) FROM authority_state WHERE id=1;",
    )?;
    Ok(file)
}

/// `holdRestoreGuard`: hold the server guard of `database` exclusively.
fn hold_guard(database: &Path) -> Result<Fixture> {
    let path = PathBuf::from(format!("{}.server-lock.sqlite", database.to_string_lossy()));
    let guard = Fixture::open(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    guard.execute_batch("BEGIN EXCLUSIVE")?;
    Ok(guard)
}

fn failed(outcome: &Outcome) -> bool {
    outcome.is_err()
}

fn error_text(outcome: &Outcome) -> String {
    match outcome {
        Ok(value) => format!("succeeded: {value}"),
        Err(value) => value["error"].as_str().unwrap_or_default().to_owned(),
    }
}

#[tokio::test]
async fn signed_infrastructure_fence_preflight_survives_restart_and_exact_retry_without_activating_restore(
) -> Result<()> {
    let mut f = Fence::new(|_, _| Ok(())).await?;
    let request = f.request()?;
    let evidence = f.signed(&request, json!({}));
    let before = f.db()?.row("SELECT * FROM authority_state", [])?;
    let result = f.verify(&evidence)?.map_err(|e| format!("verify: {e}"))?;
    ensure!(
        result
            == json!({"state": "read_only_recovery", "fenceVerified": true,
                "requestId": request["requestId"], "fenceId": "fixture-fence-1",
                "expiresAt": evidence["expiresAt"]}),
        "verify result: {result}"
    );
    ensure!(
        f.db()?.row("SELECT * FROM authority_state", [])? == before,
        "verification changed authority"
    );
    f.reopen()?;
    ensure!(
        f.verify(&evidence)?.ok() == Some(result.clone()),
        "verify replay"
    );
    ensure!(
        f.db()?.scalar::<i64>("SELECT count(*) FROM tasks", [])? == 0,
        "tasks created"
    );
    f.db()?.execute_batch(
        "CREATE TRIGGER reject_fence_request BEFORE UPDATE ON restore_fence_request \
         BEGIN SELECT RAISE(ABORT,'fixture request failure'); END",
    )?;
    ensure!(failed(&f.prepare()?), "request update ignored the fault");
    ensure!(
        f.verify(&evidence)?.ok() == Some(result),
        "verification lost after a failed request"
    );
    f.db()?.execute_batch("DROP TRIGGER reject_fence_request")?;
    f.request()?;
    let stale = f.verify(&evidence)?;
    ensure!(
        error_text(&stale).contains("Infrastructure fence evidence"),
        "stale evidence: {}",
        error_text(&stale)
    );
    ensure!(f.serving_state()? == "read_only_recovery", "serving state");
    Ok(())
}

#[tokio::test]
async fn restore_fence_rejects_forged_stale_mismatched_self_supplied_and_replaced_session_proofs(
) -> Result<()> {
    let f = Fence::new(|_, _| Ok(())).await?;
    let request = f.request()?;
    let evidence = f.signed(&request, json!({}));
    let wrong = FenceKey::generate()?;
    let now = restore::now();
    let mut forged = evidence.clone();
    forged["signature"] = json!(wrong.sign(&evidence_body(&evidence)));
    let mut padded = evidence.clone();
    padded["signature"] = json!(format!(
        "{}=",
        evidence["signature"].as_str().unwrap_or_default()
    ));
    let created = request["createdAt"].as_i64().unwrap_or_default();
    let inputs = vec![
        forged,
        padded,
        with(
            &evidence,
            json!({"signingPublicKey": f.trust["signingPublicKey"]}),
        ),
        with(&evidence, json!({"processFenced": false})),
        with(&evidence, json!({"routeFenced": false})),
        with(&evidence, json!({"admissionsFenced": false})),
        f.signed(
            &with(&request, json!({"endpoint": "https://other.example"})),
            json!({}),
        ),
        f.signed(&with(&request, json!({"controllerId": "other"})), json!({})),
        f.signed(&with(&request, json!({"targetId": "other"})), json!({})),
        f.signed(
            &with(&request, json!({"sourceAuthority": OTHER_ID})),
            json!({}),
        ),
        f.signed(
            &with(&request, json!({"recoveryGeneration": OTHER_ID})),
            json!({}),
        ),
        f.signed(
            &with(&request, json!({"ownerAccount": OTHER_ID})),
            json!({}),
        ),
        f.signed(&with(&request, json!({"requestId": OTHER_ID})), json!({})),
        f.signed(
            &with(&request, json!({"challenge": sha256_hex(new_id())})),
            json!({}),
        ),
        f.signed(&request, json!({"observedAt": created - 1})),
        f.signed(&request, json!({"observedAt": now + 60_000})),
        f.signed(&request, json!({"expiresAt": now - 1})),
        f.signed(&request, json!({"expiresAt": now + 120_000})),
    ];
    for (index, input) in inputs.iter().enumerate() {
        ensure!(
            failed(&f.verify(input)?),
            "forged evidence {index} accepted: {input}"
        );
    }
    for pin in [
        with(&f.trust, json!({"signingPublicKey": wrong.public_spki()})),
        with(&f.trust, json!({"endpoint": "https://other.example"})),
        with(&f.trust, json!({"targetId": "other"})),
    ] {
        ensure!(
            failed(&f.verify_as(&evidence, &pin, &f.owner, &f.session_file)?),
            "pin accepted: {pin}"
        );
    }
    ensure!(
        failed(&f.verify_as(&evidence, &Value::Null, &f.owner, &f.session_file)?),
        "missing pin accepted"
    );
    let mut foreign = f.owner.clone();
    foreign.issuer = "https://other.example".into();
    ensure!(
        failed(&f.verify_as(&evidence, &f.trust, &foreign, &f.session_file)?),
        "foreign issuer accepted"
    );
    let replacement = f.replace_session()?;
    ensure!(
        failed(&f.verify(&evidence)?),
        "replaced session still verifies"
    );
    let replacement_file = f.dir.join("replacement.json");
    write(&replacement_file, &replacement)?;
    ensure!(
        failed(&f.verify_as(
            &evidence,
            &f.trust,
            &f.owner,
            &replacement_file.to_string_lossy()
        )?),
        "replacement session verifies an older request"
    );
    ensure!(f.serving_state()? == "read_only_recovery", "serving state");
    Ok(())
}

#[tokio::test]
async fn operator_cli_creates_and_verifies_bounded_private_fence_files_while_keeping_recovery_read_only(
) -> Result<()> {
    let f = Fence::new(|_, _| Ok(())).await?;
    write(&f.trust_file, &f.trust)?;
    let db = f.db_path();
    let evidence_path = f.evidence_file.to_string_lossy().into_owned();
    let session = f.session_file.clone();
    let prepare_args = [
        "fence-request",
        "--db",
        db.as_str(),
        "--session",
        session.as_str(),
    ];
    let verify_args = [
        "verify-fence",
        "--db",
        db.as_str(),
        "--session",
        session.as_str(),
        "--evidence",
        evidence_path.as_str(),
    ];
    let run = |args: &[&str], subject: &str| -> Result<Value> {
        let mut actor = f.owner.clone();
        actor.subject = subject.into();
        f.run(args, &actor)
    };
    {
        let _lock = hold_guard(&f.database)?;
        let busy = run(&prepare_args, "user_owner")?;
        ensure!(busy["exitCode"] == 1, "busy: {busy}");
        ensure!(
            busy["error"]
                .as_str()
                .is_some_and(|error| error.to_lowercase().contains("canonical")),
            "busy error: {busy}"
        );
    }
    let prepared = run(&prepare_args, "user_owner")?;
    ensure!(prepared["exitCode"] == 0, "prepare: {prepared}");
    ensure!(
        prepared["state"] == "read_only_recovery",
        "prepare state: {prepared}"
    );
    let request_file = prepared["requestFile"].as_str().ok_or("request file")?;
    ensure!(
        fs::metadata(request_file)?.permissions().mode() & 0o777 == 0o600,
        "request file mode"
    );
    let session_secret = f.proof["session"].as_str().ok_or("session proof")?;
    ensure!(
        !prepared.to_string().contains(session_secret),
        "session in output"
    );
    let request = read_json(request_file)?;
    write(&f.evidence_file, &f.signed(&request, json!({})))?;
    let verified = run(&verify_args, "user_owner")?;
    ensure!(verified["exitCode"] == 0, "verify: {verified}");
    ensure!(verified["fenceVerified"] == true, "fence: {verified}");
    ensure!(
        run(&verify_args, "user_owner")? == verified,
        "verify replay"
    );
    let link = f.dir.join("proof-link.json");
    std::os::unix::fs::symlink(&f.evidence_file, &link)?;
    let link_path = link.to_string_lossy().into_owned();
    let mut linked = verify_args;
    linked[6] = link_path.as_str();
    ensure!(
        run(&linked, "user_owner")?["exitCode"] == 1,
        "symlinked evidence accepted"
    );
    fs::set_permissions(&f.evidence_file, fs::Permissions::from_mode(0o644))?;
    ensure!(
        run(&verify_args, "user_owner")?["exitCode"] == 5,
        "public evidence accepted"
    );
    fs::set_permissions(&f.evidence_file, fs::Permissions::from_mode(0o600))?;
    write_text(&f.evidence_file, &"x".repeat(65_537))?;
    ensure!(
        run(&verify_args, "user_owner")?["exitCode"] == 5,
        "oversized evidence accepted"
    );
    write(
        &f.evidence_file,
        &with(
            &f.signed(&request, json!({})),
            json!({"routeFenced": false}),
        ),
    )?;
    ensure!(
        run(&verify_args, "user_owner")?["exitCode"] == 1,
        "unfenced route accepted"
    );
    ensure!(
        run(&["activate", "--db", db.as_str()], "user_owner")?["exitCode"] == 2,
        "activate without arguments"
    );
    ensure!(f.serving_state()? == "read_only_recovery", "serving state");
    write(&f.evidence_file, &f.signed(&request, json!({})))?;
    let mut plan_args = verify_args;
    plan_args[0] = "activation-plan";
    let planned = run(&plan_args, "user_owner")?;
    ensure!(planned["exitCode"] == 0, "plan: {planned}");
    let plan_file = planned["planFile"].as_str().ok_or("plan file")?.to_owned();
    let plan = read_json(&plan_file)?;
    ensure!(
        fs::metadata(&plan_file)?.permissions().mode() & 0o777 == 0o600,
        "plan file mode"
    );
    ensure!(
        !fs::read_to_string(&plan_file)?.contains(session_secret),
        "session in plan"
    );
    let activate_args = [
        "activate",
        "--db",
        db.as_str(),
        "--session",
        session.as_str(),
        "--evidence",
        evidence_path.as_str(),
        "--plan",
        plan_file.as_str(),
    ];
    let mut tampered = plan.clone();
    tampered["recovery"]["targetId"] = json!("tampered");
    write(Path::new(&plan_file), &tampered)?;
    ensure!(
        run(&activate_args, "user_owner")?["exitCode"] == 5,
        "tampered plan accepted"
    );
    write(Path::new(&plan_file), &plan)?;
    {
        let _held = hold_guard(&f.database)?;
        ensure!(
            run(&activate_args, "user_owner")?["exitCode"] == 1,
            "activate while held"
        );
    }
    let activated = run(&activate_args, "user_owner")?;
    ensure!(activated["exitCode"] == 0, "activate: {activated}");
    ensure!(
        activated["state"] == "active",
        "activate state: {activated}"
    );
    ensure!(
        run(&activate_args, "user_owner")? == activated,
        "activate replay"
    );
    ensure!(
        !activated.to_string().contains(session_secret),
        "session in activation"
    );
    ensure!(
        run(&activate_args, "user_other")?["exitCode"] == 1,
        "other user activated"
    );
    Ok(())
}

#[tokio::test]
async fn activation_plan_commits_one_fenced_recovery_receipt_and_exact_retry() -> Result<()> {
    let mut f = Fence::new(|_, _| Ok(())).await?;
    let request = f.request()?;
    // Short-lived evidence: the case waits for it to expire.
    let evidence = f.signed(&request, json!({"expiresAt": restore::now() + 5_000}));
    let plan = f.plan(&evidence)?.map_err(|e| format!("plan: {e}"))?;
    ensure!(plan["state"] == "read_only_recovery", "plan: {plan}");
    let result = f
        .activate(&evidence, &plan)?
        .map_err(|e| format!("activate: {e}"))?;
    ensure!(result["state"] == "active", "activate: {result}");
    ensure!(f.serving_state()? == "active", "serving state");
    ensure!(
        f.db()?.scalar::<String>(
            "SELECT scope FROM receipts WHERE request_id=?",
            [plan["command"]["requestId"].as_str().unwrap_or_default()],
        )? == "restore.activate",
        "receipt scope"
    );
    let cutover = f.db()?.row(
        "SELECT cutover_version,revision FROM authority_state WHERE id=1",
        [],
    )?;
    f.reopen()?;
    ensure!(
        f.activate(&evidence, &plan)?.ok() == Some(result.clone()),
        "activate replay"
    );
    ensure!(
        f.db()?.row(
            "SELECT cutover_version,revision FROM authority_state WHERE id=1",
            []
        )? == cutover,
        "replay changed the cutover"
    );
    let wait = evidence["expiresAt"].as_i64().unwrap_or_default() - restore::now() + 1;
    tokio::time::sleep(std::time::Duration::from_millis(wait.max(0) as u64)).await;
    ensure!(
        f.activate(&evidence, &plan)?.ok() == Some(result),
        "replay after evidence expiry"
    );
    let random_session = sha256_hex(new_id());
    let mut other = f.owner.clone();
    other.subject = "other".into();
    let cases = [
        (
            f.owner.clone(),
            with(&f.proof, json!({"session": random_session})),
            f.trust.clone(),
            evidence.clone(),
            plan["command"].clone(),
        ),
        (
            f.owner.clone(),
            f.proof.clone(),
            with(&f.trust, json!({"targetId": "other"})),
            evidence.clone(),
            plan["command"].clone(),
        ),
        (
            f.owner.clone(),
            f.proof.clone(),
            f.trust.clone(),
            with(&evidence, json!({"routeFenced": false})),
            plan["command"].clone(),
        ),
        (
            f.owner.clone(),
            f.proof.clone(),
            f.trust.clone(),
            evidence.clone(),
            with(&plan["command"], json!({"scopeDigest": "0".repeat(64)})),
        ),
        (
            other,
            f.proof.clone(),
            f.trust.clone(),
            evidence.clone(),
            plan["command"].clone(),
        ),
    ];
    for (index, (actor, proof, trust, input, command)) in cases.iter().enumerate() {
        ensure!(
            failed(&f.activate_as(
                input,
                &with(&plan, json!({"command": command})),
                trust,
                actor,
                proof
            )?),
            "activation variant {index} accepted"
        );
    }
    f.db()?.execute(
        "UPDATE accounts SET blocked=1 WHERE id=?",
        [&f.owner_account],
    )?;
    ensure!(
        failed(&f.activate(&evidence, &plan)?),
        "blocked owner activated"
    );
    Ok(())
}

#[tokio::test]
async fn activation_rolls_back_on_receipt_failure_and_rejects_changed_plans() -> Result<()> {
    let f = Fence::new(|_, _| Ok(())).await?;
    let request = f.request()?;
    let evidence = f.signed(&request, json!({}));
    let plan = f.plan(&evidence)?.map_err(|e| format!("plan: {e}"))?;
    let changed = with(
        &plan,
        json!({"command": with(&plan["command"], json!({"scopeDigest": "0".repeat(64)}))}),
    );
    ensure!(
        failed(&f.activate(&evidence, &changed)?),
        "changed plan accepted"
    );
    ensure!(f.serving_state()? == "read_only_recovery", "serving state");
    f.db()?.execute_batch(
        "CREATE TRIGGER fail_activation_receipt BEFORE INSERT ON receipts \
         BEGIN SELECT RAISE(ABORT,'fixture activation failure'); END",
    )?;
    ensure!(
        failed(&f.activate(&evidence, &plan)?),
        "receipt failure ignored"
    );
    ensure!(
        f.serving_state()? == "read_only_recovery",
        "activation not rolled back"
    );
    let receipts = "SELECT count(*) FROM receipts WHERE scope='restore.activate'";
    ensure!(f.db()?.scalar::<i64>(receipts, [])? == 0, "receipt kept");
    f.db()?
        .execute_batch("DROP TRIGGER fail_activation_receipt")?;
    f.db()?
        .execute_batch("UPDATE authority_state SET revision=revision+1 WHERE id=1")?;
    let outcome = f.activate(&evidence, &plan)?;
    ensure!(
        error_text(&outcome).contains("Recovery evidence changed"),
        "changed authority: {}",
        error_text(&outcome)
    );
    let revised = f
        .plan(&evidence)?
        .map_err(|e| format!("revised plan: {e}"))?;
    f.db()?.execute_batch(
        "CREATE TABLE restore_activation_proofs(request_id TEXT PRIMARY KEY REFERENCES receipts(request_id),session_hash TEXT NOT NULL,trust_digest TEXT NOT NULL,evidence_json TEXT NOT NULL); \
         CREATE TRIGGER fail_activation_proof BEFORE INSERT ON restore_activation_proofs \
         BEGIN SELECT RAISE(ABORT,'fixture proof failure'); END",
    )?;
    ensure!(
        failed(&f.activate(&evidence, &revised)?),
        "proof failure ignored"
    );
    ensure!(
        f.serving_state()? == "read_only_recovery",
        "proof failure not rolled back"
    );
    ensure!(
        f.db()?.scalar::<i64>(receipts, [])? == 0,
        "receipt kept after proof failure"
    );
    Ok(())
}

#[tokio::test]
async fn activation_retains_populated_revoked_grants_and_held_tasks_refusing_unsafe_recovery_state(
) -> Result<()> {
    let (member, installation, device, agent, task) =
        (new_id(), new_id(), new_id(), new_id(), new_id());
    let now = restore::now();
    let f = Fence::new(|db, owner| {
        let authority: String = db.scalar("SELECT generation FROM authority_state", [])?;
        db.execute(
            "INSERT INTO accounts(id,issuer,clerk_sub,created) VALUES (?,?,?,?)",
            (&member, minarbot_testkit::canonical::ISSUER, "user_member", now),
        )?;
        db.execute(
            "INSERT INTO project_members VALUES (?,?,'member',1,0)",
            (PROJECT, &member),
        )?;
        db.execute(
            "INSERT INTO installations VALUES (?,?,?,?,1,0,1,?)",
            (&installation, owner, "fixture-key", sha256_hex("installation"), now),
        )?;
        db.execute(
            "INSERT INTO devices VALUES (?,?,?,?,?,?,?, ?,1,1,'ready','Asia/Jakarta',1,?,'[]',1)",
            rusqlite_params::devices(
                &device,
                &installation,
                owner,
                &json!({"authorityGeneration": authority}).to_string(),
                now,
            ),
        )?;
        db.execute(
            "INSERT INTO device_sessions VALUES (?,?,1,?,?,0)",
            (sha256_hex("old-session"), &device, &authority, now + 60_000),
        )?;
        db.execute(
            "INSERT INTO agents(id,name,model,instructions) VALUES (?,?,?,?)",
            (&agent, "Agent", "fixture", "Help"),
        )?;
        db.execute(
            "INSERT INTO agent_projects VALUES (?,?,'','{\"tools\":[]}',1,0)",
            (PROJECT, &agent),
        )?;
        db.execute(
            "INSERT INTO collaborator_agents VALUES (?,?,?,1,1)",
            (PROJECT, &member, &agent),
        )?;
        db.execute(
            "INSERT INTO turns(id,agent,text,status,created) VALUES (?,?,'Recover','running','2026-11-25T00:00:00.000Z')",
            (&task, &agent),
        )?;
        db.execute(
            "INSERT INTO tasks VALUES (?,?,NULL,?,?,?,?,1,1,?,'{\"tools\":[]}',NULL,NULL,NULL,'running',?,'{}',1,?)",
            rusqlite_params::tasks(&task, PROJECT, &agent, owner, &device, now),
        )?;
        db.execute(
            "INSERT INTO execution_owners VALUES (?,?,?,1,1,?,'{}',0,?,0)",
            (&task, &device, &authority, sha256_hex("executor"), now),
        )?;
        db.execute(
            "INSERT INTO routines(id,agent,name,prompt,next_at,enabled) VALUES(?,?,'Fixture','Fixture',0,1)",
            (new_id(), &agent),
        )?;
        Ok(())
    })
    .await?;
    let evidence = f.signed(&f.request()?, json!({}));
    let initial = f.plan(&evidence)?.map_err(|e| format!("plan: {e}"))?;
    let recovery = &initial["recovery"];
    for field in [
        "revokedMemberships",
        "revokedAssignments",
        "revokedDevices",
        "revokedSessions",
    ] {
        ensure!(
            recovery[field].as_array().map(Vec::len) == Some(1),
            "{field}: {recovery}"
        );
    }
    ensure!(
        recovery["heldTasks"][0]["state"] == "uncertain",
        "held task: {recovery}"
    );
    for (unsafe_state, reset) in [
        (
            "UPDATE project_members SET deleted=0 WHERE role='member'",
            "UPDATE project_members SET deleted=1 WHERE role='member'",
        ),
        (
            "UPDATE collaborator_agents SET enabled=1",
            "UPDATE collaborator_agents SET enabled=0",
        ),
        (
            "UPDATE device_sessions SET revoked=0",
            "UPDATE device_sessions SET revoked=1",
        ),
        (
            "UPDATE devices SET status='ready'",
            "UPDATE devices SET status='revoked'",
        ),
        (
            "UPDATE execution_owners SET stopped=0",
            "UPDATE execution_owners SET stopped=1",
        ),
        (
            "UPDATE tasks SET state='queued'",
            "UPDATE tasks SET state='uncertain'",
        ),
        ("UPDATE turns SET paused=0", "UPDATE turns SET paused=1"),
        (
            "UPDATE routines SET enabled=1",
            "UPDATE routines SET enabled=0",
        ),
    ] {
        f.db()?.execute_batch(unsafe_state)?;
        ensure!(failed(&f.plan(&evidence)?), "plan accepted {unsafe_state}");
        ensure!(
            failed(&f.activate(&evidence, &initial)?),
            "activation accepted {unsafe_state}"
        );
        f.db()?.execute_batch(reset)?;
    }
    let result = f
        .activate(&evidence, &initial)?
        .map_err(|e| format!("activate: {e}"))?;
    ensure!(result["state"] == "active", "activate: {result}");
    let db = f.db()?;
    ensure!(
        db.scalar::<String>("SELECT state FROM tasks", [])? == "uncertain",
        "task state"
    );
    ensure!(
        db.scalar::<i64>("SELECT stopped FROM execution_owners", [])? == 1,
        "executor"
    );
    ensure!(
        db.scalar::<i64>("SELECT enabled FROM routines", [])? == 0,
        "routine"
    );
    ensure!(
        db.scalar::<i64>(
            "SELECT deleted FROM project_members WHERE role='member'",
            []
        )? == 1,
        "member"
    );
    Ok(())
}

#[tokio::test]
async fn activation_requires_current_schema_owner_session_and_fresh_infrastructure_proof(
) -> Result<()> {
    let f = Fence::new(|_, _| Ok(())).await?;
    let request = f.request()?;
    // Short-lived evidence: the case waits for it to expire.
    let evidence = f.signed(&request, json!({"expiresAt": restore::now() + 5_000}));
    // Recovery accepts the baseline (v6) through the current schema (v7).
    // The Node case deleted v6, then the newest; removing v6 and v7 leaves the
    // database below the baseline.
    let removed = f.db()?.rows(
        "SELECT * FROM schema_migrations WHERE version>=6 ORDER BY version",
        [],
    )?;
    ensure!(removed.len() == 2, "migrations from v6: {removed:?}");
    f.db()?
        .execute_batch("DELETE FROM schema_migrations WHERE version>=6")?;
    let outdated = f.plan(&evidence)?;
    for row in &removed {
        f.db()?.execute(
            "INSERT INTO schema_migrations VALUES(?,?,?,?)",
            (
                row["version"].as_i64(),
                row["name"].as_str(),
                row["checksum"].as_str(),
                row["applied_at"].as_i64(),
            ),
        )?;
    }
    ensure!(
        error_text(&outdated).contains("Upgrade the canonical schema"),
        "outdated schema: {}",
        error_text(&outdated)
    );
    let plan = f.plan(&evidence)?.map_err(|e| format!("plan: {e}"))?;
    let expiry: i64 = f
        .db()?
        .scalar("SELECT recovery_expires_at FROM authority_state", [])?;
    f.db()?
        .execute_batch("UPDATE authority_state SET recovery_expires_at=0")?;
    let expired = f.verify(&evidence)?;
    ensure!(
        error_text(&expired).contains("Recovery evidence"),
        "expired recovery: {}",
        error_text(&expired)
    );
    f.db()?
        .execute("UPDATE authority_state SET recovery_expires_at=?", [expiry])?;
    let mut zeroed = evidence.clone();
    zeroed["signature"] = json!(minarbot_testkit::base64::Engine::encode(
        &minarbot_testkit::base64::engine::general_purpose::URL_SAFE_NO_PAD,
        [0_u8; 64]
    ));
    for input in [
        f.signed(&request, json!({"expiresAt": restore::now() - 1})),
        with(&evidence, json!({"admissionsFenced": false})),
        zeroed,
    ] {
        ensure!(failed(&f.plan(&input)?), "plan accepted {input}");
    }
    let wait = evidence["expiresAt"].as_i64().unwrap_or_default() - restore::now() + 1;
    tokio::time::sleep(std::time::Duration::from_millis(wait.max(0) as u64)).await;
    ensure!(
        failed(&f.activate(&evidence, &plan)?),
        "expired evidence activated"
    );
    let replacement = f.replace_session()?;
    ensure!(
        failed(&f.activate_as(&evidence, &plan, &f.trust, &f.owner, &replacement)?),
        "replacement session activated"
    );
    ensure!(f.serving_state()? == "read_only_recovery", "serving state");
    Ok(())
}

/// Parameter tuples longer than the borrowed literals above.
mod rusqlite_params {
    use minarbot_testkit::protocol::sha256_hex;

    pub fn devices(
        device: &str,
        installation: &str,
        owner: &str,
        authority: &str,
        now: i64,
    ) -> (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        i64,
    ) {
        (
            device.into(),
            installation.into(),
            owner.into(),
            "signing-key".into(),
            "encryption-key".into(),
            sha256_hex("signing"),
            sha256_hex("encryption"),
            authority.into(),
            now,
        )
    }

    pub fn tasks(
        task: &str,
        project: &str,
        agent: &str,
        owner: &str,
        device: &str,
        now: i64,
    ) -> (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        i64,
    ) {
        (
            task.into(),
            task.into(),
            project.into(),
            agent.into(),
            owner.into(),
            device.into(),
            sha256_hex("capabilities"),
            sha256_hex("{}"),
            now,
        )
    }
}
