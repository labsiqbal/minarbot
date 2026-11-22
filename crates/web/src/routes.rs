// Shared by the browser and Axum. Add route groups after their parity checks pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsRoute {
    General,
    Appearance,
    Providers,
    Engines,
    CraftAnything,
    Vault,
    Email,
    Hub,
    ConnectedApps,
    Mcp,
    Skills,
}

impl SettingsRoute {
    pub fn title(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Appearance => "Appearance",
            Self::Providers => "Providers",
            Self::Engines => "Engines",
            Self::CraftAnything => "Craft Anything",
            Self::Vault => "Vault",
            Self::Email => "Email",
            Self::Hub => "minarbot.com",
            Self::ConnectedApps => "Connected apps",
            Self::Mcp => "MCP connections",
            Self::Skills => "Skills",
        }
    }

    pub fn from_path(path: &str) -> Option<Self> {
        match path {
            "/settings/general" => Some(Self::General),
            "/settings/appearance" => Some(Self::Appearance),
            "/settings/providers" => Some(Self::Providers),
            "/settings/engines" => Some(Self::Engines),
            "/settings/craft-anything" => Some(Self::CraftAnything),
            "/settings/vault" => Some(Self::Vault),
            "/settings/email" => Some(Self::Email),
            "/settings/hub" => Some(Self::Hub),
            "/settings/connected-apps" => Some(Self::ConnectedApps),
            "/settings/mcp" => Some(Self::Mcp),
            "/settings/skills" => Some(Self::Skills),
            _ => None,
        }
    }
}
