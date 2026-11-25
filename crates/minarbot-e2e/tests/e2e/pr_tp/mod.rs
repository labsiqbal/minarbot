//! Hosted server with device homes, ported from `app/two-process.test.ts`
//! (#89). A canonical-mode server runs with Clerk session tokens; Rust device
//! hosts enroll, connect through a device fetch proxy (where the Node
//! suite's `fetch` patches become fault hooks) and run tasks.

#[allow(dead_code)]
mod crypto;
mod events;
mod execution;
#[allow(dead_code)]
mod fixture;
mod routines;
mod schedules;
mod shared;
mod sync;
mod workflow_config;
mod workflow_runs;
mod workflow_waits;
mod workflows;
