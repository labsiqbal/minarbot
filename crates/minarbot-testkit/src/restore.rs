//! `minarbot-server restore ...` operator commands for recovery tests (port
//! of `app/test-support/backup.ts` and `restore-identity.ts`).
//!
//! [`run`] executes one command and checks that the JSON `exitCode` equals
//! the process status. [`Identity`] supplies the Clerk environment and stdin
//! session token the owner checks need, signed with the test-only key in
//! `fixtures/clerk-test-key.pem`. [`FenceKey`] signs infrastructure fence
//! evidence with Ed25519.

use crate::{canonical::ISSUER, protocol::canonical_json, server::server_binary, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

const PRIVATE_KEY: &str = include_str!("../fixtures/clerk-test-key.pem");
const PUBLIC_KEY: &str = include_str!("../fixtures/clerk-test-key.pub.pem");

/// Milliseconds since the epoch (the restore CLI uses the real clock).
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

/// A Clerk user for the restore owner checks.
#[derive(Clone, Debug)]
pub struct Identity {
    pub issuer: String,
    pub subject: String,
    pub session: String,
    /// Public origin the token is issued for (`azp`).
    pub origin: String,
    /// Issuer the restore command trusts (`CLERK_ISSUER`).
    pub trusted_issuer: String,
}

impl Identity {
    pub fn new(subject: &str, origin: &str) -> Self {
        Self {
            issuer: ISSUER.into(),
            subject: subject.into(),
            session: format!("sess_{subject}"),
            origin: origin.into(),
            trusted_issuer: ISSUER.into(),
        }
    }

    /// Environment for the restore command.
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            ("MINARBOT_ORIGIN".into(), self.origin.clone()),
            ("CLERK_ISSUER".into(), self.trusted_issuer.clone()),
            ("CLERK_SECRET_KEY".into(), "sk_test_fixture".into()),
            ("CLERK_PUBLISHABLE_KEY".into(), "pk_test_fixture".into()),
            ("CLERK_JWT_KEY".into(), PUBLIC_KEY.into()),
            ("CLERK_AUDIENCE".into(), String::new()),
        ]
    }

    /// Stdin JSON `{token}` with a fresh RS256 session token.
    pub fn input(&self) -> Result<String> {
        let now = now() / 1000;
        let mut header = Header::new(Algorithm::RS256);
        header.typ = Some("JWT".into());
        let claims = json!({"iss": self.issuer, "sub": self.subject, "sid": self.session,
            "azp": self.origin, "exp": now + 300, "nbf": now - 1});
        let token = jsonwebtoken::encode(
            &header,
            &claims,
            &EncodingKey::from_rsa_pem(PRIVATE_KEY.as_bytes())?,
        )?;
        Ok(json!({"token": token}).to_string())
    }
}

/// Run `minarbot-server restore <args>` with `env` and optional stdin;
/// returns the JSON result after checking `exitCode` equals the status.
pub fn run(
    home: &Path,
    args: &[&str],
    env: &[(String, String)],
    input: Option<&str>,
) -> Result<Value> {
    let mut command = Command::new(server_binary(None)?);
    command
        .arg("restore")
        .args(args)
        .env_clear()
        .env("PATH", crate::server::HERMETIC_PATH)
        .env("HOME", home)
        .env("TMPDIR", home)
        .env("LANG", "C.UTF-8")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in env {
        command.env(name, value);
    }
    let mut child = command.spawn()?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin.write_all(input.as_bytes())?;
    }
    let output = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(stdout.trim()).map_err(|error| {
        format!(
            "restore {args:?} printed no JSON ({error}): {stdout} {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })?;
    let status = output.status.code().map(i64::from);
    if value["exitCode"].as_i64() != status {
        return Err(format!(
            "restore {args:?}: exitCode {} but status {status:?}",
            value["exitCode"]
        )
        .into());
    }
    Ok(value)
}

/// Ed25519 key that signs infrastructure fence evidence.
pub struct FenceKey(Ed25519KeyPair);

impl FenceKey {
    pub fn generate() -> Result<Self> {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
            .map_err(|_| "Ed25519 key generation failed")?;
        Ok(Self(
            Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|_| "Ed25519 key is invalid")?,
        ))
    }

    /// SubjectPublicKeyInfo DER, base64url (the trust pin format).
    pub fn public_spki(&self) -> String {
        let mut spki = vec![
            0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
        ];
        spki.extend_from_slice(self.0.public_key().as_ref());
        URL_SAFE_NO_PAD.encode(spki)
    }

    /// base64url signature over the canonical JSON of `body`.
    pub fn sign(&self, body: &Value) -> String {
        URL_SAFE_NO_PAD.encode(self.0.sign(canonical_json(body).as_bytes()).as_ref())
    }
}
