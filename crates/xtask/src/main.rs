#[cfg(unix)]
mod android;

mod desktop;

#[cfg(unix)]
pub const REPOSITORY: &str = "labsiqbal/minarbot";

#[cfg(unix)]
mod gates;

#[cfg(unix)]
mod github;

#[cfg(unix)]
mod leptos;

#[cfg(unix)]
mod preview;

#[cfg(unix)]
mod release;

#[cfg(all(unix, test))]
mod release_port;

#[cfg(unix)]
mod served;

#[cfg(unix)]
mod smoke;

#[cfg(unix)]
include!("unix.rs");

#[cfg(windows)]
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let result = match args.split_first() {
        Some((command, args)) if command == "desktop" => desktop::command(args),
        _ => Err("Windows supports cargo xtask desktop.".into()),
    };
    if let Err(error) = result {
        eprintln!("Error: {error}");
        std::process::exit(1);
    }
}
