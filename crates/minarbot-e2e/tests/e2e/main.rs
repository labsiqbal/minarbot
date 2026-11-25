//! End-to-end tests: one binary, one module per area. Each test starts its
//! own `minarbot-server` in a temporary root through `minarbot-testkit`;
//! agent replies come only from the kit's fake peers and scripted model.
//! The Node origin of every test is recorded in `docs/test-ledger.md`.

mod agent_create;
mod cli;
mod fx_agents;
mod fx_queues;
mod fx_support;
mod goals;
mod harness;
mod kit;
mod questions;
mod support;
mod threads;
