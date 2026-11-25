//! Provider instances and agents bound to the fake peers.
//!
//! Since #87 every agent turn runs through a provider instance. These
//! helpers create instances through `/api/provider-instances` pointing at a
//! fake binary, and agents bound to them, so tests use the same API the
//! Providers settings page uses.

use crate::{fakes, new_id, Result, Server};
use serde_json::{json, Map, Value};
use std::path::Path;

/// `POST /api/provider-instances` for `kind` (`codex`, `claude`, `acp`, ...)
/// running `binary`. Returns the saved instance.
pub async fn create_instance(
    app: &Server,
    instance: &str,
    kind: &str,
    binary: impl AsRef<Path>,
) -> Result<Value> {
    app.post_expect(
        "/api/provider-instances",
        json!({"id": new_id(), "instanceId": instance, "kind": kind, "displayName": instance,
            "enabled": true, "binaryPath": binary.as_ref()}),
        201,
    )
    .await
}

/// Instance running the built fake peer for `kind`.
pub async fn fake_instance(app: &Server, instance: &str, fake: fakes::Fake) -> Result<Value> {
    let kind = match fake {
        fakes::Fake::Codex | fakes::Fake::Gateway => "codex",
        fakes::Fake::Claude => "claude",
        fakes::Fake::Acp => "acp",
    };
    create_instance(app, instance, kind, fakes::path(fake)?).await
}

/// Fields of a new agent. `build` gives the JSON for `POST /api/agents`.
#[derive(Clone, Debug)]
pub struct AgentSpec {
    fields: Map<String, Value>,
}

impl AgentSpec {
    /// Agent with a fresh id, model `fixture` and short instructions.
    pub fn new(name: &str) -> Self {
        let mut fields = Map::new();
        fields.insert("id".into(), json!(new_id()));
        fields.insert("name".into(), json!(name));
        fields.insert("model".into(), json!("fixture"));
        fields.insert("instructions".into(), json!("Help."));
        Self { fields }
    }

    pub fn id(&self) -> &str {
        self.fields["id"].as_str().unwrap_or_default()
    }

    /// Bind to a provider instance.
    pub fn instance(self, instance: &str) -> Self {
        self.field("providerInstance", instance)
    }

    /// Test-mode runtime shortcut: `api` (the gateway) or a harness runtime.
    pub fn runtime(self, runtime: &str) -> Self {
        self.field("runtime", runtime)
    }

    pub fn field(mut self, name: &str, value: impl Into<Value>) -> Self {
        self.fields.insert(name.into(), value.into());
        self
    }

    pub fn build(&self) -> Value {
        Value::Object(self.fields.clone())
    }

    /// `POST /api/agents` expecting 201.
    pub async fn create(&self, app: &Server) -> Result<Value> {
        app.post_expect("/api/agents", self.build(), 201).await
    }
}
