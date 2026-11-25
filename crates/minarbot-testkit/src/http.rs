use crate::{server::Result, Server};
use reqwest::{header, RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value;

pub async fn expect_status(response: Response, expected: u16) -> Result<Response> {
    let actual = response.status().as_u16();
    if actual != expected {
        let url = response.url().clone();
        let body = response
            .text()
            .await
            .unwrap_or_else(|error| format!("<body read failed: {error}>"));
        return Err(format!("{url}: expected HTTP {expected}, got {actual}: {body}").into());
    }
    Ok(response)
}

pub async fn json<T: DeserializeOwned>(request: RequestBuilder, expected: u16) -> Result<T> {
    let response = expect_status(request.send().await?, expected).await?;
    Ok(response.json().await?)
}

pub async fn value(request: RequestBuilder, expected: u16) -> Result<Value> {
    json(request, expected).await
}

pub async fn get_json<T: DeserializeOwned>(server: &Server, path: &str) -> Result<T> {
    json(server.get(path), 200).await
}

pub async fn post_json<T: DeserializeOwned>(
    server: &Server,
    path: &str,
    body: &Value,
    expected: u16,
) -> Result<T> {
    json(
        server
            .post(path)
            .header(header::CONTENT_TYPE, "application/json")
            .json(body),
        expected,
    )
    .await
}

pub struct SseReader {
    stream: reqwest::Response,
    pending: Vec<u8>,
}

impl SseReader {
    pub async fn connect(server: &Server, path: &str) -> Result<Self> {
        let response = server
            .get(path)
            .header(header::ACCEPT, "text/event-stream")
            .send()
            .await?;
        let response = expect_status(response, StatusCode::OK.as_u16()).await?;
        Ok(Self {
            stream: response,
            pending: Vec::new(),
        })
    }

    pub async fn next(&mut self) -> Result<Option<Value>> {
        loop {
            if let Some(index) = self.pending.windows(2).position(|pair| pair == b"\n\n") {
                let frame: Vec<u8> = self.pending.drain(..index + 2).collect();
                let text = String::from_utf8(frame)?;
                let data = text
                    .lines()
                    .filter_map(|line| line.strip_prefix("data: "))
                    .collect::<Vec<_>>()
                    .join("\n");
                if data.is_empty() {
                    continue;
                }
                return Ok(Some(serde_json::from_str(&data)?));
            }
            match self.stream.chunk().await? {
                Some(chunk) => self.pending.extend_from_slice(&chunk),
                None => return Ok(None),
            }
        }
    }
}
