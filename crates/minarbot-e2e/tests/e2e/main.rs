//! End-to-end tests: one binary, one module per area. Each test starts its
//! own `minarbot-server` in a temporary root through `minarbot-testkit`;
//! agent replies come only from the kit's fake peers and scripted model.
//! The Node origin of every test is recorded in `docs/test-ledger.md`.

mod agent_create;
mod cli;
mod fx_acp;
mod fx_agents;
mod fx_composio;
mod fx_connectors;
mod fx_delegation;
mod fx_dispatch;
mod fx_mcp;
mod fx_queues;
mod fx_scripts;
mod fx_stream;
mod fx_support;
mod fx_tools;
mod fx_waits;
mod goals;
mod harness;
mod kit;
mod pd_agent_reply_capture;
mod pd_channel_context;
mod pd_files;
mod pd_flows_http;
mod pd_guide_infra;
mod pd_history_tools;
mod pd_image_generation;
mod pd_local_projects;
mod pd_local_routines;
mod pd_mcp_isolation;
mod pd_pdf_text;
mod pd_rust_desktop_http;
mod pd_shared_data_http;
mod pd_sidebar;
mod pd_skill_tools;
mod pd_webhooks;
mod pd_workflow_parallel;
mod pd_workspace_shell;
mod questions;
mod support;
mod threads;
