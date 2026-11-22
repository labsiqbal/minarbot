//! Index a disposable project for the parity harness.
use minarbot_codegraph::Index;
use std::{env, error::Error, ffi::OsString, io, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let args = env::args_os().skip(1).collect::<Vec<OsString>>();
    let [root, database] = args.as_slice() else {
        return Err(io::Error::other("usage: index <project-root> <database>").into());
    };
    let stats = Index::open(PathBuf::from(root), PathBuf::from(database))?.sync()?;
    println!(
        "added={} updated={} deleted={} unchanged={} issues={}",
        stats.added,
        stats.updated,
        stats.deleted,
        stats.unchanged,
        stats.issues.len()
    );
    Ok(())
}
