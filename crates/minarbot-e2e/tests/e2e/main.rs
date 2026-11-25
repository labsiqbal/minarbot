//! End-to-end tests: one binary, one module per area. Each test starts its
//! own `minarbot-server` in a temporary root through `minarbot-testkit`;
//! agent replies come only from the kit's fake peers and scripted model.
//! The Node origin of every test is recorded in `docs/test-ledger.md`.

mod agent_create;
mod cli;
mod goals;
mod harness;
mod kit;
mod questions;
mod sb_compaction;
mod sb_crash;
mod sb_flows;
mod sb_gemini;
mod sb_handoff;
mod sb_harness_default;
mod sb_http;
mod sb_mcp_context;
mod sb_memory;
mod sb_profile;
mod sb_provider;
mod sb_provider_accounts;
mod sb_provider_matrix;
mod sb_provider_settings;
mod sb_rust_harness;
mod sb_scripts;
mod sb_skills;
mod sb_slack_output;
mod sb_support;
mod sb_transcript;
mod sb_usage;
mod sb_workspace_writes;
mod support;
mod threads;
