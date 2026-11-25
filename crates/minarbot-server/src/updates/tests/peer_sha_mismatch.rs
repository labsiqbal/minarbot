//! `app/updates.test.ts` "peer SHA-256 mismatch rejects download before
//! publication" (#89): the receiver hashes the complete download and
//! refuses a corrupted archive before anything is extracted or published.
use super::*;
use axum::{body::Bytes, http::header, Router};

#[tokio::test]
async fn peer_sha256_mismatch_rejects_download_before_publication() -> Result<()> {
    const ID: &str = "1.0.0-abcdef12";
    let root = Temporary::new(&std::env::temp_dir(), "peer-sha-mismatch")?;
    let releases = root.0.join("releases");
    let release = releases.join(ID);
    fs::create_dir_all(&release).map_err(|_| storage())?;
    fs::write(release.join("package.json"), br#"{"version":"1.0.0"}"#).map_err(|_| storage())?;
    let files = minarbot_launcher::files::manifest(&release).map_err(|_| storage())?;
    let info = serde_json::json!({"id": ID, "version": "1.0.0", "files": files});
    fs::write(release.join("release.json"), info.to_string()).map_err(|_| storage())?;
    let archive = create(&releases, ID).await?;
    let mut bytes = fs::read(&archive.file).map_err(|_| storage())?;
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    let bytes = Bytes::from(bytes);
    let sha256 = archive.sha256.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| storage())?;
    let url = format!("http://{}", listener.local_addr().map_err(|_| storage())?);
    let router = Router::new().fallback(move || {
        let (bytes, sha256) = (bytes.clone(), sha256.clone());
        async move {
            (
                [
                    (header::CONTENT_TYPE.as_str(), "application/gzip".to_owned()),
                    (header::CONTENT_LENGTH.as_str(), bytes.len().to_string()),
                    ("x-minarbot-sha256", sha256),
                ],
                bytes,
            )
        }
    });
    let server = tokio::spawn(async move { axum::serve(listener, router).await });
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .map_err(PeerError::network)?;
    let received = root.0.join("received");
    let result = receive(
        &received,
        Download::Peer {
            id: ID,
            sha256: &archive.sha256,
        },
        |_| {
            let request = client.get(&url);
            async move { request.send().await.map_err(PeerError::network) }
        },
        || async { Ok(()) },
    )
    .await;
    server.abort();
    let failure = match result {
        Ok(_) => return Err(error(500, "corrupted archive was accepted")),
        Err(failure) => failure,
    };
    if !failure.message.contains("SHA-256 mismatch") {
        return Err(error(
            500,
            format!("unexpected failure: {}", failure.message),
        ));
    }
    if received.join(ID).exists() {
        return Err(error(500, "corrupted release was published"));
    }
    Ok(())
}
