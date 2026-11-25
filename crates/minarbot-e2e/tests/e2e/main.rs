//! End-to-end tests: one binary, one module per area. Each test starts its
//! own `minarbot-server` in a temporary root through `minarbot-testkit`;
//! agent replies come only from the kit's fake peers and scripted model.
//! The Node origin of every test is recorded in `docs/test-ledger.md`.

mod agent_create;
mod cli;
mod goals;
mod harness;
mod kit;
mod pr_fanout;
mod pr_github;
mod pr_hub;
mod pr_machines;
mod pr_peer_transfer;
mod pr_remote_agents;
mod pr_room_chat;
mod pr_support;
mod pr_tp;
mod pr_updates;
mod questions;
mod support;
mod threads;
