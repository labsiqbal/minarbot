// Shared by the browser and Axum. Add route groups after their parity checks pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsRoute {
    General,
    Appearance,
}

impl SettingsRoute {
    pub fn title(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Appearance => "Appearance",
        }
    }

    pub fn from_path(path: &str) -> Option<Self> {
        match path {
            "/settings/general" => Some(Self::General),
            "/settings/appearance" => Some(Self::Appearance),
            _ => None,
        }
    }
}
