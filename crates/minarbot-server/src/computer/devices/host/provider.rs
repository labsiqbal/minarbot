#![allow(dead_code)]

use super::*;
use crate::data::providers::Options;

pub(super) enum Provider {
    Configured { options: Box<Options> },
    Fixture(String),
}

pub(super) struct Call {
    endpoint: String,
    body: Value,
}

fn unavailable() -> Failure {
    fail(
        503,
        "provider_unavailable",
        "Home device provider is not available yet. Configure a provider instance on the host.",
    )
}

impl Provider {
    pub(super) fn new(fixture: Option<String>, can_use_fixture: bool) -> db::Result<Self> {
        if let Some(origin) = fixture {
            if !can_use_fixture
                || !origin.starts_with("http://127.0.0.1:")
                || !valid_origin(&origin, true)
            {
                return Err("Fixture provider requires a loopback Rust test server".into());
            }
            return Ok(Self::Fixture(origin));
        }
        let mut options = Options::environment()?;
        options.validate()?;
        Ok(Self::Configured {
            options: Box::new(options),
        })
    }

    pub(super) fn metadata(&self) -> Result<Option<(Value, bool)>> {
        Ok(None)
    }

    pub(super) fn prepare(&self, binding: &str, context: &Value) -> Result<Call> {
        let Self::Fixture(endpoint) = self else {
            let _ = (binding, context);
            return Err(unavailable());
        };
        Ok(Call {
            endpoint: endpoint.clone(),
            body: context.clone(),
        })
    }
}

impl Call {
    pub(super) async fn send(self, client: Client) -> Result<Value> {
        let response = client
            .post(&self.endpoint)
            .json(&self.body)
            .send()
            .await
            .map_err(|_| unavailable())?;
        if !response.status().is_success() {
            return Err(unavailable());
        }
        let mut bytes = Vec::new();
        let mut response = response;
        while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
            if bytes.len() + chunk.len() > RESULT_LIMIT {
                return Err(invalid());
            }
            bytes.extend_from_slice(&chunk);
        }
        crate::web::parse_json(&bytes).map_err(|_| invalid())
    }
}

fn invalid() -> Failure {
    fail(
        502,
        "provider_result_invalid",
        "Provider did not return a valid task result.",
    )
}
