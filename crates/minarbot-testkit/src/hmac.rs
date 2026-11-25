//! HMAC-SHA256 (RFC 2104) for signing webhook deliveries in tests.

use sha2::{Digest, Sha256};

/// Lowercase hex HMAC-SHA256 of the concatenated `parts` under `key`.
pub fn sha256_hex(key: &[u8], parts: &[&[u8]]) -> String {
    const BLOCK: usize = 64;
    let mut block = [0_u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|byte| byte ^ 0x36));
    for part in parts {
        inner.update(part);
    }
    let mut outer = Sha256::new();
    outer.update(block.map(|byte| byte ^ 0x5c));
    outer.update(inner.finalize());
    outer
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn matches_rfc_4231_case_2() -> crate::Result<()> {
        let mac = super::sha256_hex(b"Jefe", &[b"what do ya want ", b"for nothing?"]);
        crate::ensure!(
            mac == "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
            "mac {mac}"
        );
        Ok(())
    }
}
