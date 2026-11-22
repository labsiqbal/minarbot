use gloo_net::http::Request;
use leptos::prelude::window;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

pub async fn read<T: DeserializeOwned>(path: &str) -> Result<T, String> {
    read_status(path).await.map_err(|(_, message)| message)
}

pub async fn read_status<T: DeserializeOwned>(path: &str) -> Result<T, (u16, String)> {
    let response = Request::get(path)
        .send()
        .await
        .map_err(|_| (0, "Minarbot connection lost. Try again.".to_owned()))?;
    let status = response.status();
    decode(response).await.map_err(|message| (status, message))
}

pub async fn command<T: DeserializeOwned>(path: &str, mut body: Value) -> Result<T, String> {
    let id = window()
        .crypto()
        .map_err(|_| "Command could not be created. Reload Minarbot and try again.")?
        .random_uuid();
    body["id"] = Value::String(id);
    post(path, &body).await
}

pub async fn post<T: DeserializeOwned>(path: &str, body: &impl Serialize) -> Result<T, String> {
    let response = Request::post(path)
        .json(body)
        .map_err(|error| error.to_string())?
        .send()
        .await
        .map_err(|_| "Minarbot connection lost. Try again.")?;
    decode(response).await
}

pub async fn response_error(response: gloo_net::http::Response, fallback: &str) -> String {
    let status = response.status();
    match response.json::<Value>().await {
        Ok(body) => body["error"]
            .as_str()
            .or_else(|| body["error"]["message"].as_str())
            .unwrap_or(fallback)
            .to_owned(),
        Err(_) if matches!(status, 502..=504) => {
            "Minarbot is restarting. Try again in a moment.".into()
        }
        Err(_) => fallback.to_owned(),
    }
}

async fn decode<T: DeserializeOwned>(response: gloo_net::http::Response) -> Result<T, String> {
    if !response.ok() {
        let fallback = format!("HTTP {}", response.status());
        return Err(response_error(response, &fallback).await);
    }
    response
        .json()
        .await
        .map_err(|_| "Minarbot returned an invalid response. Try again.".into())
}
