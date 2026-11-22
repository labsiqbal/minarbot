// Shared by the browser and Axum. Add route groups after their parity checks pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsRoute {
    General,
    Appearance,
    Machines,
    Devices,
    Computer,
    Updates,
    Usage,
    Data,
    Backup,
    External,
}

impl SettingsRoute {
    pub fn title(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Appearance => "Appearance",
            Self::Machines => "Machines",
            Self::Devices => "Devices",
            Self::Computer => "Computer",
            Self::Updates => "Updates",
            Self::Usage => "Usage",
            Self::Data => "Shared data",
            Self::Backup => "Backup & restore",
            Self::External => "External access",
        }
    }

    pub fn from_path(path: &str) -> Option<Self> {
        match path {
            "/settings/general" => Some(Self::General),
            "/settings/appearance" => Some(Self::Appearance),
            "/settings/machines" => Some(Self::Machines),
            "/settings/devices" => Some(Self::Devices),
            "/settings/computer" => Some(Self::Computer),
            "/settings/updates" => Some(Self::Updates),
            "/settings/usage" => Some(Self::Usage),
            "/settings/data" => Some(Self::Data),
            "/settings/backup" => Some(Self::Backup),
            "/settings/mcp-server" => Some(Self::External),
            _ => None,
        }
    }
}
