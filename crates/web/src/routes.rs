// Shared by the browser and Axum. Add route groups after their parity checks pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsRoute {
    General,
    Appearance,
    Other(String),
}

impl SettingsRoute {
    pub fn title(&self) -> String {
        match self {
            Self::General => "General".into(),
            Self::Appearance => "Appearance".into(),
            Self::Other(id) => match id.as_str() {
                "providers" => "Providers",
                "engines" => "Engines",
                "skills" => "Skills",
                "connected-apps" => "Connected apps",
                "hub" => "minarbot.com",
                "email" => "Email",
                "mcp" => "MCP connections",
                "vault" => "Vault",
                "craft-anything" => "Craft Anything",
                "machines" => "Machines",
                "devices" => "Devices",
                "computer" => "Computer",
                "updates" => "Updates",
                "usage" => "Usage",
                "data" => "Shared data",
                "backup" => "Backup & restore",
                "mcp-server" => "External access",
                _ => "Settings",
            }
            .into(),
        }
    }

    pub fn from_path(path: &str) -> Option<Self> {
        match path {
            "/settings/general" => Some(Self::General),
            "/settings/appearance" => Some(Self::Appearance),
            path if [
                "providers",
                "engines",
                "skills",
                "connected-apps",
                "hub",
                "email",
                "mcp",
                "vault",
                "craft-anything",
                "machines",
                "devices",
                "computer",
                "updates",
                "usage",
                "data",
                "backup",
                "mcp-server",
            ]
            .iter()
            .any(|id| path == format!("/settings/{id}")) =>
            {
                Some(Self::Other(path.trim_start_matches("/settings/").into()))
            }
            _ => None,
        }
    }
}
