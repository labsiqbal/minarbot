//! End-to-end tests: one binary, one module per area. Each test starts its
//! own `minarbot-server` in a temporary root through `minarbot-testkit`;
//! agent replies come only from the kit's fake peers and scripted model.
//! The Node origin of every test is recorded in `docs/test-ledger.md`.

mod agent_create;
mod cli;
mod goals;
mod harness;
mod kit;
mod pd_files;
mod pd_flows_http;
mod pd_history_tools;
mod pd_image_generation;
mod pd_pdf_text;
mod pd_shared_data_http;
mod pd_skill_tools;
mod pd_webhooks;
mod pd_workflow_parallel;
mod questions;
mod support;
mod threads;
