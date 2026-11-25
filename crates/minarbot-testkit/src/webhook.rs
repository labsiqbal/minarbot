//! Signed webhook deliveries: HMAC-SHA256 signatures and a delivery whose
//! last body byte is held back, so a test can change the hook between
//! "request started" and "request finished" and still read the JSON reply.

use crate::Result;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

/// Lowercase hex HMAC-SHA256 of `message` under `key` (RFC 2104).
pub fn hmac_sha256_hex(key: impl AsRef<[u8]>, message: impl AsRef<[u8]>) -> String {
    const BLOCK: usize = 64;
    let key = key.as_ref();
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let pad = |byte: u8| block.iter().map(|value| value ^ byte).collect::<Vec<u8>>();
    let inner = Sha256::new()
        .chain_update(pad(0x36))
        .chain_update(message.as_ref())
        .finalize();
    Sha256::new()
        .chain_update(pad(0x5c))
        .chain_update(inner)
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A POST whose final body byte is sent only by [`HeldPost::finish`].
pub struct HeldPost {
    stream: TcpStream,
    rest: Vec<u8>,
}

impl HeldPost {
    /// Send everything but the last body byte of `POST url` with `headers`.
    pub async fn start(url: &str, headers: &[(&str, &str)], body: &[u8]) -> Result<Self> {
        let rest = url
            .strip_prefix("http://")
            .ok_or("HeldPost needs an http:// URL")?;
        let (address, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        let path = if path.is_empty() { "/" } else { path };
        let mut stream = TcpStream::connect(address).await?;
        let mut head = format!(
            "POST {path} HTTP/1.1\r\nHost: {address}\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for (name, value) in headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        let split = body.len().saturating_sub(1);
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(&body[..split]).await?;
        stream.flush().await?;
        Ok(Self {
            stream,
            rest: body[split..].to_vec(),
        })
    }

    /// Send the held byte; returns the status and the JSON body.
    pub async fn finish(mut self) -> Result<(u16, Value)> {
        self.stream.write_all(&self.rest).await?;
        self.stream.flush().await?;
        let mut response = Vec::new();
        self.stream.read_to_end(&mut response).await?;
        let text = String::from_utf8_lossy(&response).into_owned();
        let (head, body) = text
            .split_once("\r\n\r\n")
            .ok_or_else(|| format!("incomplete HTTP response: {text}"))?;
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|status| status.parse().ok())
            .ok_or_else(|| format!("no HTTP status in response: {text}"))?;
        let chunked = head.lines().any(|line| {
            line.to_ascii_lowercase()
                .starts_with("transfer-encoding: chunked")
        });
        let body = if chunked {
            dechunk(body)?
        } else {
            body.to_owned()
        };
        let value = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&body)?
        };
        Ok((status, value))
    }
}

fn dechunk(mut body: &str) -> Result<String> {
    let mut out = String::new();
    loop {
        let (size, rest) = body.split_once("\r\n").ok_or("bad chunk header")?;
        let size = usize::from_str_radix(size.trim(), 16)?;
        if size == 0 {
            return Ok(out);
        }
        out.push_str(rest.get(..size).ok_or("short chunk")?);
        body = rest.get(size + 2..).ok_or("short chunk")?;
    }
}

#[cfg(test)]
mod tests {
    use super::hmac_sha256_hex;

    /// RFC 4231 test case 2.
    #[test]
    fn hmac_matches_rfc_4231() -> crate::Result<()> {
        let digest = hmac_sha256_hex("Jefe", "what do ya want for nothing?");
        crate::ensure!(
            digest == "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
            "digest: {digest}"
        );
        Ok(())
    }
}
