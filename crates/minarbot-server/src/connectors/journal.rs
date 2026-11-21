use super::*;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub struct Journal {
    pub db: Connection,
    lock: PathBuf,
    owned: fs::Metadata,
}
impl Journal {
    pub fn open(directory: &Path, platform: &str) -> crate::db::Result<Self> {
        if !directory.is_absolute() || !["slack", "telegram"].contains(&platform) {
            return Err("Connector state directory must be absolute".into());
        }
        match fs::DirBuilder::new().mode(0o700).create(directory) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.into()),
        }
        let stat = fs::symlink_metadata(directory)?;
        if !stat.is_dir()
            || stat.uid() != rustix::process::geteuid().as_raw()
            || stat.mode() & 0o077 != 0
        {
            return Err("Connector state directory must be private and owned".into());
        }
        let lock = directory.join("owner.lock");
        let mut owner = OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .mode(0o600)
            .open(&lock)?;
        let owned = owner.metadata()?;
        let result = (|| -> crate::db::Result<Connection> {
            writeln!(owner, "{}", std::process::id())?;
            owner.sync_all()?;
            let file = directory.join(format!("{platform}.sqlite"));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
                .open(&file)
            {
                Ok(_) => (),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(e) => return Err(e.into()),
            }
            for suffix in ["", "-wal", "-shm", "-journal"] {
                match fs::symlink_metadata(minarbot_launcher::files::sidecar(&file, suffix)) {
                    Ok(stat) => {
                        if !stat.is_file()
                            || stat.nlink() != 1
                            || stat.uid() != rustix::process::geteuid().as_raw()
                            || stat.mode() & 0o077 != 0
                        {
                            return Err("Connector journal file must be private and regular".into());
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                    Err(e) => return Err(e.into()),
                }
            }
            let db = Connection::open(file)?;
            db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
            Ok(db)
        })();
        match result {
            Ok(db) => Ok(Self { db, lock, owned }),
            Err(e) => {
                unlock(&lock, &owned);
                Err(e)
            }
        }
    }
}
fn unlock(path: &Path, owned: &fs::Metadata) {
    if let Ok(stat) = fs::symlink_metadata(path) {
        if stat.ino() == owned.ino() && stat.dev() == owned.dev() {
            let _ = fs::remove_file(path);
        }
    }
}
impl Drop for Journal {
    fn drop(&mut self) {
        unlock(&self.lock, &self.owned);
    }
}
pub fn private_file(path: &Path, limit: u64) -> crate::db::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)?;
    let stat = file.metadata()?;
    if !stat.is_file()
        || stat.uid() != rustix::process::geteuid().as_raw()
        || stat.mode() & 0o077 != 0
        || stat.len() > limit
    {
        return Err("Script discovery is unavailable or unsafe.".into());
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn refuses_concurrent_ownership_and_unsafe_journal_files() -> crate::db::Result<()> {
        let root = std::env::temp_dir().join(format!("minarbot-journal-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root)?;
        let result = (|| -> crate::db::Result<()> {
            for platform in ["slack", "telegram"] {
                let directory = root.join(platform);
                let first = Journal::open(&directory, platform)?;
                assert!(Journal::open(&directory, platform).is_err());
                assert!(directory.join("owner.lock").exists());
                drop(first);
                assert!(!directory.join("owner.lock").exists());
                drop(Journal::open(&directory, platform)?);
                let outside = root.join("outside");
                fs::write(&outside, "KEEP")?;
                let database = directory.join(format!("{platform}.sqlite"));
                fs::remove_file(&database)?;
                symlink(&outside, database)?;
                let rejected = match Journal::open(&directory, platform) {
                    Err(error) => error,
                    Ok(_) => panic!("unsafe journal opened"),
                };
                assert!(rejected.to_string().contains("private and regular"));
                assert!(!directory.join("owner.lock").exists());
                assert_eq!(fs::read(&outside)?, b"KEEP");
            }
            Ok(())
        })();
        fs::remove_dir_all(root)?;
        result
    }
}
