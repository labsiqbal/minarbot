//! Model discovery validates the catalog and keeps provider secrets out of
//! errors (port of `app/models.test.ts`). Since #87 the catalog comes from the
//! agent's provider instance: the server asks the harness (`model/list`),
//! which reads the provider's `GET /models`.

use crate::sa_support::{lock, RawProvider, Shared};
use minarbot_testkit::{prelude::*, server::Gateway};

const INSTANCE: &str = "codex-test-gateway";

#[tokio::test]
async fn model_discovery_validates_catalog_and_never_follows_credential_redirects() -> Result<()> {
    let mode: Shared<String> = std::sync::Arc::new(std::sync::Mutex::new("ok".into()));
    let current = mode.clone();
    let provider = RawProvider::start(move |request| {
        let mode = current.lock().map(|mode| mode.clone()).unwrap_or_default();
        if request.path == "/unexpected" {
            return (200, vec![], Vec::new());
        }
        let json = |data: Value| {
            (
                200,
                vec![("content-type".into(), "application/json".into())],
                json!({"data": data}).to_string().into_bytes(),
            )
        };
        match mode.as_str() {
            "error" => (
                401,
                vec![],
                b"private provider detail fixture-secret".to_vec(),
            ),
            "large" => (200, vec![], vec![b'x'; 2 * 1024 * 1024 + 1]),
            "bad-json" => (200, vec![], b"invalid".to_vec()),
            "invalid" => json(json!([{"id": "\n"}])),
            "spaces" => json(json!([{"id": " model "}])),
            "empty" => json(json!([])),
            _ => json(json!([{"id": "z"}, {"id": "a"}, {"id": "z"}])),
        }
    })
    .await?;
    let root = TestRoot::new()?;
    let app = root
        .start(ServerConfig::new().gateway(Gateway {
            api_key: Some("fixture-secret".into()),
            default_model: Some("z".into()),
            ..Gateway::new(provider.base_url())
        }))
        .await?;
    let path = format!("/api/provider-instances/{INSTANCE}/models");
    let body = json!({"id": new_id(), "instanceId": INSTANCE});
    let catalog = app.post_expect(&path, body.clone(), 200).await?;
    ensure!(
        catalog["models"] == json!(["z", "a"]),
        "deduplicated models: {catalog}"
    );
    let ids: Vec<Value> = catalog["catalog"]
        .as_array()
        .map(|rows| rows.iter().map(|row| row["id"].clone()).collect())
        .unwrap_or_default();
    ensure!(
        ids == [json!("z"), json!("a"), json!("z")],
        "catalog rows: {catalog}"
    );
    ensure!(
        catalog["catalog"][0]["default"] == true && catalog["catalog"][0]["label"] == "z",
        "default row: {catalog}"
    );
    for failure in ["error", "large", "bad-json", "invalid", "spaces"] {
        *lock(&mode)? = failure.into();
        let reply = app
            .call(
                minarbot_testkit::http::Method::POST,
                &path,
                Some(body.clone()),
            )
            .await?;
        ensure!(
            reply.status == 502,
            "{failure}: {} {}",
            reply.status,
            reply.body
        );
        let text = reply.body.to_string();
        ensure!(
            !text.contains("private provider detail") && !text.contains("fixture-secret"),
            "{failure} leaked: {text}"
        );
    }
    let requests = provider.requests();
    ensure!(requests.len() == 6, "provider calls: {requests:?}");
    ensure!(
        requests.iter().all(|request| request.method == "GET"
            && request.path == "/v1/models"
            && request.authorization.as_deref() == Some("Bearer fixture-secret")),
        "provider requests: {requests:?}"
    );
    *lock(&mode)? = "empty".into();
    ensure!(
        app.post_expect(&path, body.clone(), 200).await? == json!({"models": [], "catalog": []}),
        "empty catalog"
    );
    Ok(())
}
