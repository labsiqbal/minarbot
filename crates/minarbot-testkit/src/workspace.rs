//! Temporary directories for agent workspaces and fixture files.

use crate::Result;
use std::{
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

/// A canonicalized temporary directory removed on drop.
pub struct TempWorkspace {
    path: PathBuf,
}

impl TempWorkspace {
    /// New directory under the system temp directory.
    pub fn new() -> Result<Self> {
        Self::under(std::env::temp_dir())
    }

    /// New directory under `parent` (created if missing).
    pub fn under(parent: impl AsRef<Path>) -> Result<Self> {
        let parent = parent.as_ref();
        fs::create_dir_all(parent)?;
        let path = parent.join(format!("minarbot-workspace-{}", Uuid::new_v4()));
        fs::create_dir(&path)?;
        Ok(Self {
            path: path.canonicalize()?,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn join(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.path.join(relative)
    }

    /// Write `contents` to `relative`, creating parent directories.
    pub fn write(&self, relative: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> Result<PathBuf> {
        let path = self.path.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, contents)?;
        Ok(path)
    }

    pub fn read(&self, relative: impl AsRef<Path>) -> Result<String> {
        Ok(fs::read_to_string(self.path.join(relative))?)
    }

    pub fn exists(&self, relative: impl AsRef<Path>) -> bool {
        self.path.join(relative).exists()
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
