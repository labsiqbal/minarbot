//! Signatures and the wait-secret reveal of protected waits.

use minarbot_testkit::{
    base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine},
    prelude::*,
};
use ring::{aead, agreement, hkdf, hmac, rand::SystemRandom};

/// Hex HMAC-SHA256 of `message` with `secret`.
pub fn hmac_hex(secret: &str, message: &str) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    hmac::sign(&key, message.as_bytes())
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

const X25519_SPKI: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x6e, 0x03, 0x21, 0x00,
];

/// Browser X25519 key pair for a reveal request.
pub struct Browser {
    private: Option<agreement::EphemeralPrivateKey>,
    /// SPKI DER of the public key, base64url.
    pub public: String,
}

impl Browser {
    pub fn new() -> Result<Self> {
        let private =
            agreement::EphemeralPrivateKey::generate(&agreement::X25519, &SystemRandom::new())
                .map_err(|_| "x25519 key")?;
        let public = private
            .compute_public_key()
            .map_err(|_| "x25519 public key")?;
        let mut der = X25519_SPKI.to_vec();
        der.extend_from_slice(public.as_ref());
        Ok(Self {
            private: Some(private),
            public: URL_SAFE_NO_PAD.encode(der),
        })
    }

    /// ECDH with the device's SPKI encryption key; consumes the private key.
    pub fn shared(&mut self, device_spki: &str) -> Result<Vec<u8>> {
        let der = URL_SAFE_NO_PAD.decode(device_spki)?;
        let peer = agreement::UnparsedPublicKey::new(
            &agreement::X25519,
            der.get(12..).ok_or("short device key")?.to_vec(),
        );
        let private = self.private.take().ok_or("browser key already used")?;
        agreement::agree_ephemeral(private, &peer, |shared| shared.to_vec())
            .map_err(|_| "x25519 agreement".into())
    }
}

struct Len32;

impl hkdf::KeyType for Len32 {
    fn len(&self) -> usize {
        32
    }
}

/// Decrypt a reveal (`aes-256-gcm` with HKDF-SHA256(shared, salt=requestId,
/// info='minarbot-wait-reveal-v1') and the bound associated data).
pub fn decrypt_reveal(
    shared: &[u8],
    request_id: &str,
    value: &Value,
    owner: &str,
) -> Result<String> {
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, request_id.as_bytes());
    let prk = salt.extract(shared);
    let mut key = [0_u8; 32];
    prk.expand(&[b"minarbot-wait-reveal-v1"], Len32)
        .map_err(|_| "hkdf")?
        .fill(&mut key)
        .map_err(|_| "hkdf fill")?;
    let aad = json!([
        value["requestId"],
        value["waitKey"],
        owner,
        value["deviceGeneration"],
        value["secretGeneration"],
        value["expiresAt"]
    ])
    .to_string();
    let text = |name: &str| URL_SAFE_NO_PAD.decode(value[name].as_str().unwrap_or_default());
    let mut sealed = text("ciphertext")?;
    sealed.extend(text("tag")?);
    let nonce = aead::Nonce::try_assume_unique_for_key(&text("iv")?).map_err(|_| "nonce")?;
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_256_GCM, &key).map_err(|_| "aes key")?,
    );
    let plain = key
        .open_in_place(nonce, aead::Aad::from(aad.as_bytes()), &mut sealed)
        .map_err(|_| "reveal does not decrypt")?;
    Ok(String::from_utf8(plain.to_vec())?)
}

/// Sorted keys of a JSON object.
pub fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .map(|fields| fields.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

/// The same names, sorted.
pub fn sorted(names: &[&str]) -> Vec<String> {
    let mut names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    names.sort();
    names
}
