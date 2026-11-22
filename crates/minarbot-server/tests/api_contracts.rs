#[path = "api_contracts/auth.rs"]
mod auth;
#[path = "api_contracts/automation.rs"]
mod automation;
#[path = "api_contracts/connectors.rs"]
mod connectors;
#[path = "api_contracts/harness.rs"]
mod harness;
#[path = "support/loopback.rs"]
mod loopback;
#[path = "support/server.rs"]
mod server;
#[path = "api_contracts/storage.rs"]
mod storage;
#[path = "api_contracts/updates.rs"]
mod updates;

use reqwest::RequestBuilder;
use serde_json::Value;
use server::{Result, WAIT};
use std::time::Duration;
use tokio::time::{sleep, timeout};
use uuid::Uuid;

fn id() -> String {
    Uuid::new_v4().to_string()
}

async fn json(request: RequestBuilder, expected: u16) -> Result<Value> {
    let response = request.send().await?;
    let status = response.status().as_u16();
    let url = response.url().clone();
    assert_eq!(status, expected, "{url}");
    Ok(response.json().await?)
}

async fn until<T>(mut check: impl AsyncFnMut() -> Result<Option<T>>) -> Result<T> {
    timeout(WAIT, async {
        loop {
            if let Some(value) = check().await? {
                return Result::Ok(value);
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await?
}
