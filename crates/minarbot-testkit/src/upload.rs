//! Raw uploads the regular client cannot send: a chunked request body with
//! no `Content-Length`, written chunk by chunk.

use crate::Result;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

/// `POST path` to `origin` (`http://127.0.0.1:<port>`) with
/// `Transfer-Encoding: chunked`, sending each of `chunks` as one chunk.
/// The server may answer and close before the body ends (for example with
/// 413), so write errors after the headers are ignored and the status is
/// read from whatever the server sent.
pub async fn chunked_post(
    origin: &str,
    path: &str,
    headers: &[(&str, &str)],
    chunks: impl IntoIterator<Item = Vec<u8>>,
) -> Result<u16> {
    let address = origin
        .strip_prefix("http://")
        .ok_or("chunked_post needs an http:// origin")?;
    let mut stream = TcpStream::connect(address).await?;
    let mut head = format!(
        "POST {path} HTTP/1.1\r\nHost: {address}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n"
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await?;
    for chunk in chunks {
        let frame = [
            format!("{:x}\r\n", chunk.len()).into_bytes(),
            chunk,
            b"\r\n".to_vec(),
        ]
        .concat();
        if stream.write_all(&frame).await.is_err() {
            break;
        }
    }
    let _ = stream.write_all(b"0\r\n\r\n").await;
    let _ = stream.flush().await;
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response).await;
    let text = String::from_utf8_lossy(&response);
    text.split_whitespace()
        .nth(1)
        .and_then(|status| status.parse().ok())
        .ok_or_else(|| format!("no HTTP status in response: {text}").into())
}
