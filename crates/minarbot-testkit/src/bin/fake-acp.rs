//! Fake Agent Client Protocol peer. See `minarbot_testkit::fake::acp`.
use minarbot_testkit::fake::{self, acp};

fn main() -> std::process::ExitCode {
    fake::main(acp::main)
}
