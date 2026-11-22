use crate::connection::{origin, Result};
use serde::{Deserialize, Deserializer, Serialize};
use std::{fs, io, io::Write, path::Path};
use url::Url;

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
pub struct Bounds {
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub servers: Vec<String>,
    pub active_server: Option<String>,
    #[serde(default, deserialize_with = "read_bounds")]
    pub window: Option<Bounds>,
    #[serde(default)]
    pub managed_local: bool,
}
fn read_bounds<'de, D: Deserializer<'de>>(
    decoder: D,
) -> std::result::Result<Option<Bounds>, D::Error> {
    let value = serde_json::Value::deserialize(decoder)?;
    Ok(serde_json::from_value::<Bounds>(value)
        .ok()
        .filter(|bounds| bounds.width >= 640 && bounds.height >= 480))
}
impl Settings {
    pub fn load(path: &Path, electron: &Path) -> Result<Self> {
        if let Some(settings) = read(path)? {
            return Ok(settings);
        }
        let settings = read(electron)?.unwrap_or_default();
        settings.save(path)?;
        Ok(settings)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        let directory = path.parent().ok_or("Missing settings directory.")?;
        fs::create_dir_all(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer_pretty(&mut file, self)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|error| error.error)?;
        #[cfg(unix)]
        fs::File::open(directory)?.sync_all()?;
        Ok(())
    }
    pub fn select(&mut self, url: &Url, managed_local: bool) {
        let url = url.to_string();
        if !self.servers.contains(&url) {
            self.servers.push(url.clone());
        }
        self.active_server = Some(url);
        self.managed_local = managed_local;
    }
    pub fn remove(&mut self, url: &str) {
        self.servers.retain(|saved| saved != url);
        if self.active_server.as_deref() == Some(url) {
            self.active_server = self.servers.first().cloned();
            self.managed_local = false;
        }
    }
}
fn read(path: &Path) -> Result<Option<Settings>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let settings: Settings = serde_json::from_slice(&bytes)
        .map_err(|_| "Invalid settings.json. Repair its JSON before restarting.")?;
    if settings
        .servers
        .iter()
        .any(|saved| !origin(saved).is_ok_and(|normalized| normalized.as_str() == saved))
        || settings
            .active_server
            .as_ref()
            .is_some_and(|active| !settings.servers.contains(active))
    {
        return Err("Invalid settings.json. Repair its server addresses before restarting.".into());
    }
    Ok(Some(settings))
}
#[cfg(test)]
#[test]
fn import_once_and_leave_electron_usable() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let legacy = temp.path().join("electron/settings.json");
    let path = temp.path().join("tauri/settings.json");
    let mut electron = Settings::default();
    electron.select(&origin("https://server.example")?, false);
    electron.save(&legacy)?;
    let mut imported = Settings::load(&path, &legacy)?;
    assert_eq!(imported.active_server, electron.active_server);
    imported.remove("https://server.example/");
    imported.save(&path)?;
    assert!(Settings::load(&path, &legacy)?.servers.is_empty());
    assert_eq!(
        read(&legacy)?.and_then(|v| v.active_server),
        electron.active_server
    );
    Ok(())
}
