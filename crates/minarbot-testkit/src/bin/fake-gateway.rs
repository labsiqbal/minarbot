//! Fake Codex gateway peer backed by a scripted model. See
//! `minarbot_testkit::fake::gateway`.
use minarbot_testkit::fake::{self, gateway};

fn main() -> std::process::ExitCode {
    fake::main(gateway::main)
}
