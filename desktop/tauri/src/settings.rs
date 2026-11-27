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
    pub fn load(path: &Path, legacy: &Path) -> Result<Self> {
        if let Some(settings) = read(path)? {
            return Ok(settings);
        }
        let settings = read(legacy)?.unwrap_or_default();
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
mod tests {
    use super::*;

    #[test]
    fn imports_legacy_profile_once() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let legacy = temp.path().join("minarbot-desktop/settings.json");
        let path = temp.path().join("com.minarbot.desktop.tauri/settings.json");
        let original = r#"{
            "servers": ["https://nuc.example:4317/", "http://127.0.0.1:4317/"],
            "activeServer": "https://nuc.example:4317/",
            "window": {"x": 5, "y": 8, "width": 900, "height": 700}
        }"#;
        fs::create_dir_all(legacy.parent().ok_or("Missing legacy directory")?)?;
        fs::write(&legacy, original)?;

        let mut imported = Settings::load(&path, &legacy)?;
        assert_eq!(
            imported.servers,
            ["https://nuc.example:4317/", "http://127.0.0.1:4317/"]
        );
        assert_eq!(
            imported.active_server.as_deref(),
            Some("https://nuc.example:4317/")
        );
        assert!(!imported.managed_local);
        let window = imported.window.ok_or("Missing imported bounds")?;
        assert_eq!(
            (window.x, window.y, window.width, window.height),
            (Some(5), Some(8), 900, 700)
        );
        imported.remove("https://nuc.example:4317/");
        imported.save(&path)?;

        let reopened = Settings::load(&path, &legacy)?;
        assert_eq!(
            reopened.active_server.as_deref(),
            Some("http://127.0.0.1:4317/")
        );
        assert_eq!(reopened.servers, ["http://127.0.0.1:4317/"]);
        assert_eq!(fs::read_to_string(&legacy)?, original);
        Ok(())
    }

    #[test]
    fn persists_selection_and_bounds_and_preserves_broken_settings() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("settings.json");
        let legacy = temp.path().join("legacy.json");
        let mut settings = Settings::load(&path, &legacy)?;
        settings.select(&origin("https://nuc.example:4317")?, false);
        settings.select(&origin("https://other.example")?, false);
        settings.window = Some(Bounds {
            x: Some(4),
            y: Some(8),
            width: 900,
            height: 700,
        });
        settings.save(&path)?;

        let mut saved = Settings::load(&path, &legacy)?;
        assert_eq!(
            serde_json::to_value(&saved)?,
            serde_json::to_value(&settings)?
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
        }
        saved.remove("https://other.example/");
        assert_eq!(
            saved.active_server.as_deref(),
            Some("https://nuc.example:4317/")
        );
        saved.remove("https://nuc.example:4317/");
        assert!(saved.active_server.is_none());

        fs::write(&path, "{broken")?;
        assert!(Settings::load(&path, &legacy).is_err());
        assert_eq!(fs::read_to_string(&path)?, "{broken");
        Ok(())
    }
}
