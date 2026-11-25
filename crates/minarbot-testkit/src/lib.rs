//! Rust-only integration-test support for Minarbot.

pub mod fixtures;
pub mod http;
pub mod server;
pub mod wait;

pub use server::{Result, Server, ServerConfig, TestRoot, WAIT};
pub type TestServerConfig = ServerConfig;
