//! Fake Claude Code stream-json peer. See `minarbot_testkit::fake::native`.
use minarbot_testkit::fake::{self, native};

fn main() -> std::process::ExitCode {
    fake::main(|args| native::main(native::Mode::Claude, args))
}
