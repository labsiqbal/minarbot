use serde::Deserialize;
use std::{
    env,
    io::Read,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthConfig {
    pub secret_key: String,
    pub publishable_key: String,
    pub issuer: String,
    pub origin: String,
    pub audience: Option<String>,
    #[serde(default)]
    pub development: bool,
    pub jwt_key: Option<String>,
    pub api_url: Option<String>,
}

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AppearanceClient {
    #[default]
    React,
    Leptos,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub database: PathBuf,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_dist")]
    pub dist: PathBuf,
    #[serde(default)]
    pub appearance_client: AppearanceClient,
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    pub auth: Option<AuthConfig>,
    pub fixed_clock: Option<i64>,
    #[serde(default)]
    pub test_remote_pairing: bool,
    pub tls: Option<Tls>,
    #[serde(default, deserialize_with = "workspace_bindings")]
    pub workspaces: Vec<WorkspaceConfig>,
    #[serde(flatten)]
    pub data: crate::data::Options,
    #[serde(flatten)]
    pub computer: crate::computer::options::Options,
    #[serde(flatten)]
    pub hub: crate::computer::hub_config::Config,
    #[serde(flatten)]
    pub update: crate::updates::config::Config,
    #[serde(flatten)]
    pub remaining_data: crate::data::options::Options,
    #[serde(flatten)]
    pub turns: crate::turns::options::Options,
    #[serde(flatten)]
    pub media: crate::media::Options,
    #[serde(flatten)]
    pub connectors: crate::connectors::config::Options,
    #[serde(flatten)]
    pub automation: crate::automation::options::Options,
    #[serde(flatten)]
    pub harness: crate::harness::config::Config,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfig {
    pub agent: String,
    pub directory: PathBuf,
    #[serde(default)]
    pub write: bool,
    #[serde(skip)]
    pub managed: bool,
}
fn workspace_value(value: serde_json::Value) -> crate::db::Result<Vec<WorkspaceConfig>> {
    let rows = value
        .as_array()
        .filter(|rows| rows.len() <= 256)
        .ok_or("Workspace bindings allow at most 256 entries")?;
    rows.iter()
        .map(|row| {
            serde_json::from_value(row.clone()).map_err(|_| "Invalid workspace binding".into())
        })
        .collect()
}
fn workspace_bindings<'de, D: serde::Deserializer<'de>>(
    input: D,
) -> Result<Vec<WorkspaceConfig>, D::Error> {
    workspace_value(serde_json::Value::deserialize(input)?).map_err(serde::de::Error::custom)
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tls {
    pub cert: String,
    pub key: String,
}
fn default_port() -> u16 {
    4317
}
fn default_host() -> String {
    "127.0.0.1".to_owned()
}
pub fn local_database() -> PathBuf {
    std::env::var_os("MINARBOT_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var_os("HOME").unwrap_or_default())
                .join(".local/share/minarbot/new-app")
        })
        .join(minarbot_launcher::DATABASE_NAME)
}
fn default_dist() -> PathBuf {
    if let Ok(executable) = env::current_exe() {
        if let Some(root) = executable.parent().and_then(|directory| directory.parent()) {
            for dist in [root.join("app/dist"), root.join("dist")] {
                if dist.is_dir() {
                    return dist;
                }
            }
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../app/dist")
}
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}
pub fn trusted_origin(value: &str, development: bool) -> crate::db::Result<()> {
    let url = url::Url::parse(value).map_err(|_| "Invalid public origin")?;
    if url.origin().ascii_serialization() != value
        || !url.username().is_empty()
        || url.password().is_some()
        || url.scheme() != "https"
            && !(development
                && url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")))
    {
        return Err("Public origin must be an exact HTTPS origin".into());
    }
    Ok(())
}
impl Config {
    pub fn now(&self) -> i64 {
        self.fixed_clock.unwrap_or_else(now)
    }
    pub fn read() -> crate::db::Result<Self> {
        let args: Vec<_> = env::args().skip(1).collect();
        if args == ["--test-config"] {
            if std::env::var("MINARBOT_TEST_SERVER").as_deref() != Ok("rust") {
                return Err("--test-config requires MINARBOT_TEST_SERVER=rust".into());
            }
            let mut input = String::new();
            std::io::stdin()
                .take(1024 * 1024)
                .read_to_string(&mut input)?;
            let mut config: Self = serde_json::from_str(&input)?;
            config.harness.test_mode = true;
            config.validate()?;
            return Ok(config);
        }
        if args.iter().any(|arg| arg != "--mode=canonical") {
            return Err("Unsupported Minarbot mode".into());
        }
        let port = env::var("PORT")
            .unwrap_or_else(|_| "4317".to_owned())
            .parse::<u16>()
            .map_err(|_| "PORT must be 1..65535")?;
        if port == 0 {
            return Err("PORT must be 1..65535".into());
        }
        let required =
            |key| std::env::var(key).map_err(|_| format!("{key} is required for canonical mode"));
        let canonical_mode = args.iter().any(|arg| arg == "--mode=canonical");
        let tls = match (
            std::env::var("MINARBOT_TLS_CERT").ok(),
            std::env::var("MINARBOT_TLS_KEY").ok(),
        ) {
            (Some(cert), Some(key)) => Some(Tls {
                cert: std::fs::read_to_string(cert)?,
                key: std::fs::read_to_string(key)?,
            }),
            (None, None) => None,
            _ => return Err("Canonical TLS requires both certificate and key".into()),
        };
        let auth = if canonical_mode {
            let server_url = required("MINARBOT_SERVER_URL")?;
            let development = url::Url::parse(&server_url)?.scheme() == "http";
            trusted_origin(&server_url, development)?;
            Some(AuthConfig {
                secret_key: required("CLERK_SECRET_KEY")?,
                publishable_key: required("CLERK_PUBLISHABLE_KEY")?,
                issuer: required("CLERK_ISSUER")?,
                origin: required("MINARBOT_ORIGIN")?,
                audience: env::var("CLERK_AUDIENCE")
                    .ok()
                    .filter(|value| !value.is_empty()),
                development,
                jwt_key: None,
                api_url: None,
            })
        } else {
            None
        };
        let database = if canonical_mode {
            PathBuf::from(required("MINARBOT_CANONICAL_DB")?)
        } else {
            local_database()
        };
        let mut config = Self {
            database,
            port,
            host: default_host(),
            dist: default_dist(),
            appearance_client: match env::var("MINARBOT_APPEARANCE_CLIENT").as_deref() {
                Ok("leptos") => AppearanceClient::Leptos,
                Ok("react") | Err(env::VarError::NotPresent) => AppearanceClient::React,
                _ => return Err("MINARBOT_APPEARANCE_CLIENT must be react or leptos".into()),
            },
            auth,
            allowed_origins: std::env::var("MINARBOT_ALLOWED_ORIGINS")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect(),
            fixed_clock: None,
            test_remote_pairing: false,
            tls,
            workspaces: match std::env::var("MINARBOT_WORKSPACES") {
                Ok(raw) if raw.len() > 65536 => {
                    return Err("Workspace configuration exceeds 64 KiB".into())
                }
                Ok(raw) => workspace_value(
                    serde_json::from_str(&raw).map_err(|_| "Invalid workspace JSON")?,
                )?,
                Err(env::VarError::NotPresent) => Vec::new(),
                Err(_) => return Err("Invalid workspace JSON".into()),
            },
            data: crate::data::Options::environment()?,
            computer: crate::computer::options::Options::environment()?,
            hub: crate::computer::hub_config::Config::environment()?,
            update: crate::updates::config::Config::environment()?,
            remaining_data: crate::data::options::Options::environment(),
            turns: crate::turns::options::Options::environment()?,
            media: crate::media::Options::environment()?,
            connectors: crate::connectors::config::Options::environment(),
            automation: crate::automation::options::Options::environment(),
            harness: crate::harness::config::Config::default(),
        };
        config.validate()?;
        Ok(config)
    }
    fn validate(&mut self) -> crate::db::Result<()> {
        crate::files::validate_workspaces(&self.workspaces)?;
        self.data.validate()?;
        self.computer.validate()?;
        self.hub.validate()?;
        crate::automation::options::configure(self)?;
        self.remaining_data.validate()?;
        self.turns.validate()?;
        self.media.validate()?;
        self.connectors.validate()?;
        self.update.validate(self.harness.test_mode)?;
        if self.tls.is_none() && !matches!(self.host.as_str(), "127.0.0.1" | "::1") {
            return Err("HTTP development listener must use loopback".into());
        }
        if self.database.as_os_str().is_empty() || self.database.to_str() == Some(":memory:") {
            return Err("Rust server needs a file database".into());
        }
        for origin in &self.allowed_origins {
            trusted_origin(origin, false)?;
        }
        if let Some(auth) = &self.auth {
            if auth.secret_key.is_empty() || auth.publishable_key.is_empty() {
                return Err("Clerk configuration is required".into());
            }
            trusted_origin(&auth.origin, auth.development)?;
            let issuer =
                url::Url::parse(&auth.issuer).map_err(|_| "Invalid Clerk issuer configuration")?;
            if !issuer.username().is_empty()
                || issuer.password().is_some()
                || issuer.query().is_some()
                || issuer.fragment().is_some()
                || issuer.scheme() != "https"
                    && !(auth.development
                        && issuer.scheme() == "http"
                        && matches!(issuer.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")))
            {
                return Err("Invalid Clerk issuer configuration".into());
            }
            if !auth.development && self.tls.is_none() {
                return Err("Canonical server requires TLS outside development".into());
            }
            if let Some(api) = &auth.api_url {
                trusted_origin(api, auth.development)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flattened_data_options_reject_unknown_keys_and_validate_credentials() -> crate::db::Result<()>
    {
        let base = json!({"database":"/tmp/config-fixture.sqlite","scriptToken":"fixture-script-token-abcdefghijklmnopqrstuvwxyz","testKeyring":{}});
        let mut config: Config = serde_json::from_value(base.clone())?;
        config.validate()?;
        let mut invalid = base.clone();
        invalid["unportedOption"] = json!(true);
        assert!(serde_json::from_value::<Config>(invalid).is_err());
        for token in ["", "short", "a token with whitespace 1234567890123456789"] {
            let mut invalid = base.clone();
            invalid["scriptToken"] = json!(token);
            let mut config: Config = serde_json::from_value(invalid)?;
            assert!(config.validate().is_err());
        }
        Ok(())
    }

    #[test]
    fn shared_config_normalizes_accounts_and_preserves_workspace_errors() -> crate::db::Result<()> {
        let directory = env::temp_dir();
        let agent = uuid::Uuid::new_v4().to_string();
        let base = json!({"database":directory.join("config-test.sqlite"),"workspaces":[{"agent":agent,"directory":directory,"write":true}],"accounts":[{"id":"fixture","name":" Fixture ","baseURL":"http://127.0.0.1:1234"}]});
        let mut config: Config = serde_json::from_value(base.clone())?;
        config.validate()?;
        assert_eq!(config.workspaces.len(), 1);
        assert!(config.workspaces[0].write);
        for (field, value, message) in [
            (
                "workspaces",
                json!(null),
                "Workspace bindings allow at most 256 entries",
            ),
            (
                "workspaces",
                json!([{"agent":agent,"directory":directory,"write":1}]),
                "Invalid workspace binding",
            ),
            (
                "workspaces",
                json!([{"agent":agent,"directory":"relative"}]),
                "Invalid workspace binding",
            ),
            (
                "workspaces",
                json!([{"agent":agent,"directory":format!("{}/../missing",directory.display())}]),
                "Invalid workspace binding",
            ),
            (
                "accounts",
                json!(null),
                "Provider accounts must be an array of at most 32 entries",
            ),
            (
                "accounts",
                json!([{"id":"fixture","name":"Fixture","baseURL":"http://foreign.example"}]),
                "Invalid provider account URL",
            ),
            (
                "accounts",
                json!([{"id":"fixture","name":"Fixture","baseURL":"https://example.test","provider":1}]),
                "MINARBOT_PROVIDER must be openai-compatible, anthropic or gemini",
            ),
            (
                "accounts",
                json!([{"id":"fixture","name":"Fixture","baseURL":"https://example.test","streamTermination":1}]),
                "MINARBOT_STREAM_TERMINATION must be marker or finish-reason",
            ),
        ] {
            let mut input = base.clone();
            input[field] = value;
            let result = serde_json::from_value::<Config>(input)
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })
                .and_then(|mut config| config.validate());
            match result {
                Err(error) => assert!(error.to_string().contains(message), "{error}"),
                Ok(()) => panic!("Invalid {field} accepted"),
            }
        }
        Ok(())
    }
}
