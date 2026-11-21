use crate::{files::remove, Result};
use std::{
    fs,
    os::unix::{fs::PermissionsExt, net::SocketAddr},
    path::{Path, PathBuf},
};
use tokio::net::UnixListener;

struct SocketPath {
    path: PathBuf,
    temporary: Option<PathBuf>,
}

impl Drop for SocketPath {
    fn drop(&mut self) {
        let _ = remove(&self.path);
        if let Some(directory) = &self.temporary {
            let _ = fs::remove_dir(directory);
        }
    }
}

pub struct ControlSocket {
    pub listener: UnixListener,
    path: SocketPath,
}

impl ControlSocket {
    pub fn bind(data: &Path) -> Result<Self> {
        let mut path = data.join("launcher.sock");
        let temporary = if SocketAddr::from_pathname(&path).is_err() {
            let directory = nix::unistd::mkdtemp("/tmp/minarbot-XXXXXX")?;
            path = directory.join("launcher.sock");
            Some(directory)
        } else {
            remove(&path)?;
            None
        };
        let path = SocketPath { path, temporary };
        let listener = UnixListener::bind(&path.path)?;
        fs::set_permissions(&path.path, fs::Permissions::from_mode(0o600))?;
        Ok(Self { listener, path })
    }

    pub fn path(&self) -> &Path {
        &self.path.path
    }
}
