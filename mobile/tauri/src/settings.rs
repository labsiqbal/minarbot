use crate::{address, connection::Result};
use serde::{Deserialize, Serialize};
use std::{fs, io, path::Path};
use url::Url;

/// The phone remembers one server: the one it connected to last.
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub server: Option<String>,
}

impl Settings {
    /// A missing or unreadable file starts fresh: the phone asks for the server again.
    pub fn load(path: &Path) -> Self {
        let settings = match fs::read(path) {
            Ok(bytes) => serde_json::from_slice::<Settings>(&bytes).unwrap_or_default(),
            Err(error) => {
                if error.kind() != io::ErrorKind::NotFound {
                    log::warn!("Could not read {}: {error}", path.display());
                }
                Settings::default()
            }
        };
        Settings {
            server: settings
                .server
                .filter(|saved| address(saved).is_ok_and(|url| url.as_str() == saved)),
        }
    }

    pub fn server(&self) -> Option<Url> {
        self.server.as_deref().and_then(|saved| address(saved).ok())
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let directory = path.parent().ok_or("Missing settings directory.")?;
        fs::create_dir_all(directory)?;
        let partial = path.with_extension("json.partial");
        fs::write(&partial, serde_json::to_vec_pretty(self)?)?;
        fs::rename(&partial, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_only_a_valid_server_origin() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        assert!(Settings::load(&path).server().is_none());
        Settings {
            server: Some("https://server.example:4317/".into()),
        }
        .save(&path)?;
        assert_eq!(
            Settings::load(&path).server().map(String::from).as_deref(),
            Some("https://server.example:4317/")
        );
        for invalid in [
            r#"{"server":"http://server.example/"}"#,
            r#"{"server":"https://server.example/path"}"#,
            "not json",
        ] {
            fs::write(&path, invalid)?;
            assert!(Settings::load(&path).server.is_none(), "{invalid}");
        }
        Ok(())
    }
}
