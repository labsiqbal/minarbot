//! HMAC-SHA256 for signed webhook and event deliveries.

use sha2::{Digest, Sha256};

/// Lowercase hex HMAC-SHA256 of `message` under `key` (RFC 2104).
pub fn hmac_sha256_hex(key: &[u8], message: &[u8]) -> String {
    const BLOCK: usize = 64;
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let pad = |byte: u8| block.iter().map(|value| value ^ byte).collect::<Vec<u8>>();
    let inner = Sha256::new()
        .chain_update(pad(0x36))
        .chain_update(message)
        .finalize();
    let outer = Sha256::new()
        .chain_update(pad(0x5c))
        .chain_update(inner)
        .finalize();
    outer.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::hmac_sha256_hex;

    #[test]
    fn rfc_4231_case_2() -> crate::Result<()> {
        let digest = hmac_sha256_hex(b"Jefe", b"what do ya want for nothing?");
        crate::ensure!(
            digest == "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
            "digest {digest}"
        );
        Ok(())
    }
}
