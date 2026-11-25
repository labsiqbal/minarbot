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
mod sa_core;
mod sa_file_output_panel;
mod sa_memory;
mod sa_memory_selection;
mod sa_memory_tools;
mod sa_models;
mod sa_routines;
mod sa_shared_data;
mod sa_support;
mod sa_workspace_git;
mod support;
mod threads;
