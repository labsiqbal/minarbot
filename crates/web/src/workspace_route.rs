// Conversation, workflow and settings ownership are handled by their own route groups.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceRoute {
    Setup,
    Project(String),
}

impl WorkspaceRoute {
    pub fn from_path(path: &str) -> Option<Self> {
        if path == "/" {
            return Some(Self::Setup);
        }
        let id = path.strip_prefix("/projects/")?;
        valid_id(id).then(|| Self::Project(id.to_ascii_lowercase()))
    }

    pub fn title(&self) -> &'static str {
        match self {
            Self::Setup => "Set up Minarbot",
            Self::Project(_) => "Project",
        }
    }
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(position, byte)| {
            if matches!(position, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}
