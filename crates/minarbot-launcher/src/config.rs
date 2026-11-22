use crate::{files::*, Result};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

pub struct Config {
    pub data: PathBuf,
    pub releases: PathBuf,
    pub port: u16,
    pub timeout: Duration,
    pub env: BTreeMap<String, String>,
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        private_file(path)?;
        let value = json(path)?;
        let object = value.as_object().ok_or("Invalid launcher configuration.")?;
        if object.keys().any(|key| {
            ![
                "dataDir",
                "envFile",
                "releasesDir",
                "port",
                "trialTimeoutMs",
            ]
            .contains(&key.as_str())
        }) || !value["dataDir"]
            .as_str()
            .is_some_and(|p| p.starts_with('/'))
            || object.contains_key("envFile")
                && !value["envFile"]
                    .as_str()
                    .is_some_and(|p| p.starts_with('/'))
        {
            return Err("Invalid launcher configuration.".into());
        }
        let number = |key, default| -> Result<u64> {
            if value[key].is_null() {
                return Ok(default);
            }
            let number = value[key]
                .as_f64()
                .ok_or("Invalid launcher port or trial timeout.")?;
            if number.fract() != 0.0 || number < 0.0 {
                return Err("Invalid launcher port or trial timeout.".into());
            }
            Ok(number as u64)
        };
        let port = number("port", 4317)?;
        let timeout = number("trialTimeoutMs", 120_000)?;
        if !(1..=65535).contains(&port) || !(500..=1_800_000).contains(&timeout) {
            return Err("Invalid launcher port or trial timeout.".into());
        }
        let data = absolute(Path::new(
            value["dataDir"]
                .as_str()
                .ok_or("Invalid launcher configuration.")?,
        ))?;
        let releases = if value["releasesDir"].is_null() {
            Path::new(&std::env::var("HOME")?).join(".local/share/minarbot/releases")
        } else {
            PathBuf::from(
                value["releasesDir"]
                    .as_str()
                    .ok_or("Invalid launcher configuration.")?,
            )
        };
        let releases = absolute(&releases)?;
        let mut env: BTreeMap<String, String> = std::env::vars_os()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect();
        env.remove("MINARBOT_LAUNCHER_ENV_FILE");
        if let Some(file) = value["envFile"].as_str() {
            private_file(Path::new(file))?;
            env.extend(parse_env(&std::fs::read_to_string(file)?));
            env.insert("MINARBOT_LAUNCHER_ENV_FILE".into(), file.into());
        }
        env.insert("PORT".into(), port.to_string());
        for (key, path) in [
            ("MINARBOT_DATA_DIR", &data),
            ("MINARBOT_RELEASES_DIR", &releases),
        ] {
            env.insert(key.into(), path.to_string_lossy().into_owned());
        }
        env.remove("MINARBOT_UPDATE_TRIAL");
        Ok(Self {
            data,
            releases,
            port: port as u16,
            timeout: Duration::from_millis(timeout),
            env,
        })
    }
}
pub fn parse_env(source: &str) -> BTreeMap<String, String> {
    let source = source.replace('\r', "");
    let mut rest = source.as_str();
    let mut values = BTreeMap::new();
    while !rest.is_empty() {
        rest = rest.trim_start_matches([' ', '\t', '\n']);
        if rest.is_empty() {
            break;
        }
        let line_end = rest.find('\n').unwrap_or(rest.len());
        let line = &rest[..line_end];
        if line.starts_with('#') || !line.contains('=') {
            rest = &rest[line_end..];
            continue;
        }
        let Some(equal) = line.find('=') else {
            break;
        };
        let key = line[..equal]
            .trim_matches([' ', '\t'])
            .strip_prefix("export ")
            .unwrap_or(line[..equal].trim_matches([' ', '\t']))
            .trim_matches([' ', '\t']);
        if key.is_empty() {
            rest = &rest[line_end..];
            continue;
        }
        let value = rest[equal + 1..].trim_start_matches([' ', '\t']);
        if let Some(quote @ ('\'' | '"' | '`')) = value.chars().next() {
            if let Some(end) = value[1..].find(quote) {
                let text = &value[1..end + 1];
                values.insert(
                    key.into(),
                    if quote == '"' {
                        text.replace("\\n", "\n")
                    } else {
                        text.into()
                    },
                );
                rest = &value[end + 2..];
                rest = &rest[rest.find('\n').unwrap_or(rest.len())..];
                continue;
            }
        }
        let end = value.find('\n').unwrap_or(value.len());
        values.insert(
            key.into(),
            value[..end]
                .split('#')
                .next()
                .unwrap_or("")
                .trim_matches([' ', '\t'])
                .into(),
        );
        rest = &value[end..];
    }
    values
}
