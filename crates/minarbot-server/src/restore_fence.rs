use crate::{
    auth::Actor,
    computer::devices::{decode, public_key},
    config::{now, trusted_origin},
    data::{all, closed, one, string, table},
    db::{canonical, digest, SCHEMA_VERSION},
    restore::{owner, random_proof, session_digest},
    web::{id, Failure},
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use uuid::Uuid;

pub const MANIFEST_LIMIT: usize = 8 * 1024 * 1024;

type Result<T> = std::result::Result<T, Failure>;

fn denied() -> Failure {
    Failure::new(
        403,
        "restore_fence_invalid",
        "Infrastructure fence evidence is invalid or expired.",
    )
}

fn hash(value: &Value) -> String {
    digest(&canonical(value))
}

fn label(value: &Value) -> Result<&str> {
    value
        .as_str()
        .filter(|s| {
            !s.trim().is_empty()
                && s.encode_utf16().count() <= 200
                && !s.bytes().any(|b| b < 32 || b == 127)
        })
        .map(str::trim)
        .ok_or_else(denied)
}

fn time(value: &Value) -> Result<i64> {
    value
        .as_i64()
        .filter(|n| (0..=9_007_199_254_740_991).contains(n))
        .ok_or_else(denied)
}

pub fn trust(input: &Value, origin: &str) -> Result<Value> {
    if !closed(
        input,
        &[
            "v",
            "endpoint",
            "controllerId",
            "targetId",
            "signingPublicKey",
        ],
    ) || input["v"] != 1
        || input["endpoint"] != origin
    {
        return Err(denied());
    }
    trusted_origin(origin, false).map_err(|_| denied())?;
    public_key(&input["signingPublicKey"], "ed25519").map_err(|_| denied())?;
    Ok(
        json!({"v":1,"endpoint":origin,"controllerId":label(&input["controllerId"])?,"targetId":label(&input["targetId"])?,"signingPublicKey":input["signingPublicKey"]}),
    )
}

pub struct Recovery<'a> {
    pub db: &'a Connection,
    pub actor: &'a Actor,
    pub session: &'a str,
    pub generation: &'a str,
    pub trust: &'a Value,
}

impl Recovery<'_> {
    fn current(&self, timestamp: i64) -> Result<Value> {
        let state = owner(self.db, self.actor, timestamp)?;
        if self.actor.expires <= timestamp
            || decode(&json!(self.session), 32).is_err()
            || state["generation"] != self.generation
            || !bool::from(
                session_digest(self.actor, &state["issuer"], self.session)
                    .as_bytes()
                    .ct_eq(string(&state, "recovery_session_hash").as_bytes()),
            )
        {
            return Err(denied());
        }
        Ok(state)
    }

    fn source(&self) -> Result<Value> {
        one(
            self.db,
            "SELECT generation FROM minarbot_backup_authority",
            [],
        )?
        .ok_or_else(denied)
    }

    pub fn prepare(&self) -> Result<Value> {
        let timestamp = now();
        let state = self.current(timestamp)?;
        let source = self.source()?;
        if source["generation"] == self.generation || id(string(&source, "generation")).is_none() {
            return Err(denied());
        }
        let challenge = random_proof().map_err(|_| Failure::storage())?;
        let request = json!({
            "v":1,"requestId":Uuid::new_v4().to_string(),"challenge":challenge,
            "endpoint":self.trust["endpoint"],"controllerId":self.trust["controllerId"],"targetId":self.trust["targetId"],
            "sourceAuthority":source["generation"],"recoveryGeneration":self.generation,"ownerAccount":self.actor.account,
            "createdAt":timestamp,"expiresAt":(timestamp+900_000).min(time(&state["recovery_expires_at"])?),
        });
        self.db.execute_batch("CREATE TABLE IF NOT EXISTS restore_fence_request(
            id INTEGER PRIMARY KEY CHECK(id=1),request_json TEXT NOT NULL CHECK(json_valid(request_json)),
            trust_digest TEXT NOT NULL,session_hash TEXT NOT NULL) STRICT;")?;
        self.db.execute("INSERT INTO restore_fence_request VALUES(1,?,?,?) ON CONFLICT(id) DO UPDATE SET request_json=excluded.request_json,trust_digest=excluded.trust_digest,session_hash=excluded.session_hash", params![canonical(&request),hash(self.trust),state["recovery_session_hash"].as_str()])?;
        Ok(request)
    }

    fn verified(&self, evidence: &Value, timestamp: i64) -> Result<()> {
        let state = self.current(timestamp)?;
        if !closed(
            evidence,
            &[
                "request",
                "fenceId",
                "processFenced",
                "routeFenced",
                "admissionsFenced",
                "observedAt",
                "expiresAt",
                "signature",
            ],
        ) || evidence["processFenced"] != true
            || evidence["routeFenced"] != true
            || evidence["admissionsFenced"] != true
            || !table(self.db, "restore_fence_request")?
        {
            return Err(denied());
        }
        let request = &evidence["request"];
        let saved = one(
            self.db,
            "SELECT * FROM restore_fence_request WHERE id=1",
            [],
        )?
        .ok_or_else(denied)?;
        let observed = time(&evidence["observedAt"])?;
        let expires = time(&evidence["expiresAt"])?;
        let request_expires = time(&request["expiresAt"])?;
        if saved["request_json"] != canonical(request)
            || saved["trust_digest"] != hash(self.trust)
            || saved["session_hash"] != state["recovery_session_hash"]
            || self.source()?["generation"] != request["sourceAuthority"]
            || request["ownerAccount"] != self.actor.account
            || request["recoveryGeneration"] != self.generation
            || request_expires <= timestamp
            || observed < time(&request["createdAt"])?
            || observed > timestamp
            || expires <= timestamp
            || expires > request_expires.min(observed + 60_000)
        {
            return Err(denied());
        }
        let body = json!([
            "minarbot-infrastructure-fence-v1",
            request,
            label(&evidence["fenceId"])?,
            true,
            true,
            true,
            observed,
            expires
        ]);
        let key = public_key(&self.trust["signingPublicKey"], "ed25519").map_err(|_| denied())?;
        let signature = decode(&evidence["signature"], 64).map_err(|_| denied())?;
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &key[12..])
            .verify(canonical(&body).as_bytes(), &signature)
            .map_err(|_| denied())
    }

    pub fn verify(&self, evidence: &Value) -> Result<Value> {
        self.verified(evidence, now())?;
        Ok(
            json!({"state":"read_only_recovery","fenceVerified":true,"requestId":evidence["request"]["requestId"],"fenceId":label(&evidence["fenceId"])?,"expiresAt":evidence["expiresAt"]}),
        )
    }

    fn manifest(&self, evidence: &Value) -> Result<Value> {
        let db = self.db;
        let schema: i64 = db.query_row(
            "SELECT coalesce(max(version),0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if !(crate::db::BASELINE_VERSION..=SCHEMA_VERSION).contains(&schema) {
            return Err(Failure::new(
                409,
                "restore_schema_upgrade_required",
                "Upgrade the canonical schema before activating recovery.",
            ));
        }
        if db.prepare("SELECT 1 FROM project_members WHERE role<>'owner' AND deleted=0 UNION ALL SELECT 1 FROM collaborator_agents WHERE enabled=1 UNION ALL SELECT 1 FROM device_sessions WHERE revoked=0 AND authority_generation<>? UNION ALL SELECT 1 FROM execution_owners WHERE stopped=0 LIMIT 1")?.exists([self.generation])? {
            return Err(denied());
        }
        for (name, predicate) in [
            ("routines", "enabled<>0"),
            ("workflow_schedules", "enabled<>0"),
            ("workflow_webhooks", "enabled<>0"),
            ("script_permissions", "enabled<>0"),
            ("mcp_connections", "enabled<>0"),
            ("turn_waits", "status='waiting'"),
            ("workflow_runs", "status IN('ready','waiting','approval')"),
            ("workflow_steps", "status IN('waiting','approval')"),
            ("flow_steps", "status IN('waiting','queued')"),
            ("provider_calls", "status='pending'"),
            ("schedules", "enabled<>0"),
            ("wait_provisions", "state IN('pending','activated')"),
            ("workflow_waits", "status='waiting'"),
        ] {
            if table(db, name)?
                && db
                    .prepare(&format!("SELECT 1 FROM {name} WHERE {predicate} LIMIT 1"))?
                    .exists([])?
            {
                return Err(denied());
            }
        }
        if db.prepare("SELECT 1 FROM invitations WHERE state<>'revoked' UNION ALL SELECT 1 FROM mail_outbox WHERE state<>'revoked' OR length(ciphertext)<>0 UNION ALL SELECT 1 FROM tasks WHERE state NOT IN('uncertain','paused','approval','completed','failed','cancelled','transferred') UNION ALL SELECT 1 FROM turns WHERE status IN('queued','running','interrupted') AND paused=0 LIMIT 1")?.exists([])? {
            return Err(denied());
        }
        let devices = all(db,"SELECT id,generation,json_extract(descriptor_json,'$.authorityGeneration') AS authorityGeneration FROM devices WHERE status<>'revoked' ORDER BY id", [])?;
        if devices
            .iter()
            .any(|device| device["authorityGeneration"] != self.generation)
        {
            return Err(denied());
        }
        let request = &evidence["request"];
        let recovery = json!({
            "fencedOldAuthority":request["sourceAuthority"],"newAuthorityGeneration":self.generation,"endpoint":request["endpoint"],
            "controllerId":request["controllerId"],"targetId":request["targetId"],"fenceId":label(&evidence["fenceId"])?,
            "authority":one(db, "SELECT generation,cutover_version,revision FROM authority_state WHERE id=1", [])?,
            "revokedMemberships":all(db,"SELECT project,account,grant_revision FROM project_members WHERE role<>'owner' ORDER BY project,account", [])?,
            "revokedAssignments":all(db,"SELECT project,account,agent,revision FROM collaborator_agents ORDER BY project,account,agent", [])?,
            "revokedSessions":all(db,"SELECT id_hash,device,generation,authority_generation FROM device_sessions WHERE revoked=1 ORDER BY id_hash", [])?,
            "revokedDevices":all(db,"SELECT id,generation FROM devices WHERE status='revoked' ORDER BY id", [])?,
            "reEnrolledDevices":devices,
            "importedJournalWatermarks":all(db,"SELECT source_device,source_authority,json_extract(envelope_json,'$.sourceDeviceGeneration') AS source_generation,max(watermark) AS watermark FROM recovery_imports WHERE recovery_generation=? GROUP BY source_device,source_authority,source_generation ORDER BY source_device,source_authority,source_generation", [self.generation])?,
            "heldTasks":all(db,"SELECT id,state,revision FROM tasks WHERE state NOT IN('completed','failed','cancelled','transferred') ORDER BY id", [])?,
        });
        if canonical(&recovery).len() > MANIFEST_LIMIT {
            return Err(Failure::new(
                413,
                "restore_scope_too_large",
                "Recovery manifest exceeds the limit.",
            ));
        }
        Ok(recovery)
    }

    pub fn plan(&self, evidence: &Value) -> Result<Value> {
        self.verified(evidence, now())?;
        let recovery = self.manifest(evidence)?;
        Ok(
            json!({"state":"read_only_recovery","command":{"v":1,"requestId":Uuid::new_v4().to_string(),"recoveryGeneration":self.generation,"scopeDigest":hash(&recovery),"evidenceDigest":hash(evidence)},"recovery":recovery}),
        )
    }

    pub fn activate(&self, evidence: &Value, plan: &Value, issuer: &str) -> Result<Value> {
        self.activate_with_clock(evidence, plan, issuer, now)
    }

    fn activate_with_clock(
        &self,
        evidence: &Value,
        plan: &Value,
        issuer: &str,
        mut clock: impl FnMut() -> i64,
    ) -> Result<Value> {
        let command = &plan["command"];
        if !closed(plan, &["state", "command", "recovery"])
            || !closed(
                command,
                &[
                    "v",
                    "requestId",
                    "recoveryGeneration",
                    "scopeDigest",
                    "evidenceDigest",
                ],
            )
            || command["v"] != 1
            || command["recoveryGeneration"] != self.generation
            || command["evidenceDigest"] != hash(evidence)
        {
            return Err(denied());
        }
        if plan["state"] != "read_only_recovery"
            || command["scopeDigest"] != hash(&plan["recovery"])
        {
            return Err(Failure::new(
                409,
                "restore_plan_manifest_invalid",
                "Activation plan manifest does not match the command.",
            ));
        }
        let request_id = id(string(command, "requestId"))
            .filter(|value| command["requestId"] == *value)
            .ok_or_else(denied)?;
        let payload = hash(&json!(["minarbot-restore-activate-v1", command]));
        let session = hash(&json!([
            "minarbot-restore-activation-session-v1",
            self.actor.account,
            issuer,
            self.actor.subject,
            self.session
        ]));
        let trust = hash(self.trust);
        let db = self.db;
        if let Some(saved) = one(
            db,
            "SELECT * FROM receipts WHERE request_id=?",
            [&request_id],
        )? {
            if saved["actor"] != self.actor.account
                || saved["scope"] != "restore.activate"
                || saved["payload_digest"] != payload
                || self.actor.expires <= clock()
                || !table(db, "restore_activation_proofs")?
            {
                return Err(denied());
            }
            let proof = one(db, "SELECT p.* FROM restore_activation_proofs p JOIN authority_state a ON a.id=1 WHERE p.request_id=? AND a.serving_state='active' AND a.generation=?", [&request_id, self.generation])?.ok_or_else(denied)?;
            if proof["trust_digest"] != trust
                || !bool::from(
                    string(&proof, "session_hash")
                        .as_bytes()
                        .ct_eq(session.as_bytes()),
                )
            {
                return Err(denied());
            }
            return serde_json::from_str(string(&saved, "result_json"))
                .map_err(|_| Failure::storage());
        }
        self.verified(evidence, clock())?;
        let recovery = self.manifest(evidence)?;
        if command["scopeDigest"] != hash(&recovery) {
            return Err(Failure::new(
                409,
                "restore_scope_changed",
                "Recovery evidence changed. Check the new activation plan.",
            ));
        }
        let committed = clock();
        self.current(committed)?;
        if time(&evidence["expiresAt"])? <= committed
            || time(&evidence["request"]["expiresAt"])? <= committed
        {
            return Err(denied());
        }
        let revision: i64 = db.query_row("UPDATE authority_state SET serving_state='active',cutover_version=cutover_version+1,revision=revision+1,recovery_session_hash=NULL,recovery_expires_at=NULL WHERE id=1 RETURNING revision", [], |row| row.get(0))?;
        let receipt = json!({"requestId":request_id,"actorId":self.actor.account,"payloadDigest":payload,"status":200,"recordId":self.generation,"recordRevision":revision,"committedAt":committed});
        let result = json!({"state":"active","recovery":recovery,"receipt":receipt});
        db.execute(
            "INSERT INTO receipts VALUES(?,?,NULL,'restore.activate',?,200,?,?)",
            params![
                request_id,
                self.actor.account,
                payload,
                canonical(&result),
                committed
            ],
        )?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS restore_activation_proofs(request_id TEXT PRIMARY KEY REFERENCES receipts(request_id),session_hash TEXT NOT NULL,trust_digest TEXT NOT NULL,evidence_json TEXT NOT NULL CHECK(json_valid(evidence_json))) STRICT;")?;
        db.execute(
            "INSERT INTO restore_activation_proofs VALUES(?,?,?,?)",
            params![request_id, session, trust, canonical(evidence)],
        )?;
        db.execute("DELETE FROM restore_fence_request", [])?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
    use ring::signature::{Ed25519KeyPair, KeyPair};

    #[test]
    fn proof_expiring_during_manifest_review_cannot_activate_recovery() -> db::Result<()> {
        let db = Connection::open_in_memory()?;
        let timestamp = now();
        db::migrate(&db, timestamp, SCHEMA_VERSION)?;
        let actor = Actor {
            account: Uuid::new_v4().to_string(),
            subject: "user_fixture".into(),
            session: "session_fixture".into(),
            expires: timestamp + 60_000,
        };
        let issuer = "https://fixture.example";
        let source = Uuid::new_v4().to_string();
        let generation = Uuid::new_v4().to_string();
        let session = random_proof()?;
        let project = Uuid::new_v4().to_string();
        db.execute(
            "INSERT INTO accounts(id,issuer,clerk_sub,created) VALUES(?,?,?,?)",
            params![actor.account, issuer, actor.subject, timestamp],
        )?;
        db.execute("INSERT INTO projects(id,account,name,private,deleted,revision) VALUES(?,?,'Fixture',1,0,1)", params![project,actor.account])?;
        db.execute(
            "INSERT INTO project_members VALUES(?,?,'owner',1,0)",
            params![project, actor.account],
        )?;
        db.execute_batch(
            "CREATE TABLE minarbot_backup_authority(generation TEXT,owner_account TEXT)",
        )?;
        db.execute(
            "INSERT INTO minarbot_backup_authority VALUES(?,?)",
            params![source, actor.account],
        )?;
        db.execute("UPDATE authority_state SET serving_state='read_only_recovery',generation=?,recovery_session_hash=?,recovery_expires_at=?",params![generation,session_digest(&actor,&json!(issuer),&session),timestamp+30_000])?;
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
            .map_err(|_| "Key generation failed")?;
        let keys = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|_| "Key opening failed")?;
        let mut spki = vec![
            0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
        ];
        spki.extend_from_slice(keys.public_key().as_ref());
        let trust = json!({"v":1,"endpoint":"https://fixture.example","controllerId":"fixture","targetId":"target","signingPublicKey":B64.encode(spki)});
        let recovery = Recovery {
            db: &db,
            actor: &actor,
            session: &session,
            generation: &generation,
            trust: &trust,
        };
        let request = recovery.prepare().map_err(|error| error.message)?;
        let observed = now();
        let expires = observed + 5_000;
        let body = json!([
            "minarbot-infrastructure-fence-v1",
            request,
            "fence",
            true,
            true,
            true,
            observed,
            expires
        ]);
        let signature = B64.encode(keys.sign(canonical(&body).as_bytes()).as_ref());
        let evidence = json!({"request":request,"fenceId":"fence","processFenced":true,"routeFenced":true,"admissionsFenced":true,"observedAt":observed,"expiresAt":expires,"signature":signature});
        let plan = recovery.plan(&evidence).map_err(|error| error.message)?;
        let mut reads = 0;
        let result = crate::data::transaction(&db, || {
            recovery.activate_with_clock(&evidence, &plan, issuer, || {
                reads += 1;
                if reads == 1 {
                    observed
                } else {
                    expires
                }
            })
        });
        assert_eq!(reads, 2);
        assert_eq!(
            result.err().map(|error| error.code),
            Some("restore_fence_invalid")
        );
        let state: String =
            db.query_row("SELECT serving_state FROM authority_state", [], |row| {
                row.get(0)
            })?;
        assert_eq!(state, "read_only_recovery");
        assert!(!db
            .prepare("SELECT 1 FROM receipts WHERE scope='restore.activate'")?
            .exists([])?);
        Ok(())
    }
}
