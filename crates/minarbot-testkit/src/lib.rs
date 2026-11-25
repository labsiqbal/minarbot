//! Dev-only integration-test kit for Minarbot.
//!
//! A test starts a disposable `minarbot-server` process in a [`TestRoot`],
//! talks to it over same-origin HTTP, and gets agent replies only from the
//! kit's fake harness peers ([`fakes`]) and, for the Codex gateway peer, from
//! a [`model::ScriptedModel`]. See `docs/testing.md` for a worked example.
//!
//! ```no_run
//! use minarbot_testkit::{prelude::*, model::{Reply, ScriptedModel}};
//! # async fn example() -> minarbot_testkit::Result<()> {
//! let model = ScriptedModel::start().await?;
//! model.respond(|request| Reply::text(format!("reply:{}", request.last_text())));
//! let root = TestRoot::new()?;
//! let app = root.start(ServerConfig::new().model(&model)).await?;
//! let agent = app.post_expect("/api/agents", json!({"id": new_id(), "runtime": "api",
//!     "name": "Agent", "model": "fixture", "instructions": "Help."}), 201).await?;
//! # Ok(()) }
//! ```

pub mod bridge;
pub mod canonical;
pub mod cli;
pub mod connector_fixture;
pub mod control;
pub mod db;
pub mod fake;
pub mod fakes;
pub mod http;
pub mod image;
pub mod issuer;
pub mod model;
pub mod protocol;
pub mod providers;
pub mod proxy;
pub mod server;
pub mod stub;
pub mod task_bridge;
pub mod wait;
pub mod workspace;

/// Boxed error used by every kit helper and by tests (`Result<()>` + `?`).
pub type Error = Box<dyn std::error::Error + Send + Sync>;
/// Result alias for kit helpers and tests.
pub type Result<T, E = Error> = std::result::Result<T, E>;

pub use server::{Server, ServerConfig, TestRoot, FIXED_CLOCK, WAIT};

/// Re-exported for tests that build fixture payloads.
pub use base64;

/// Fail the current test with a formatted message unless `condition` holds.
///
/// Tests return `Result`, so this replaces `assert!` where the message must
/// carry observed state and the caller wants `?` propagation.
#[macro_export]
macro_rules! ensure {
    ($condition:expr, $($message:tt)+) => {
        if !$condition {
            return Err(format!($($message)+).into());
        }
    };
}

/// Common imports for integration tests.
pub mod prelude {
    pub use crate::ensure;
    pub use crate::http::Reply as HttpReply;
    pub use crate::server::{Server, ServerConfig, TestRoot};
    pub use crate::wait::{until, until_value};
    pub use crate::workspace::TempWorkspace;
    pub use crate::{new_id, Result};
    pub use serde_json::{json, Value};
}

/// A fresh lowercase UUID v4, the identifier format every Minarbot command uses.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
