pub const GUIDE_PATH: &str = "/agents/00000000-0000-4000-8000-000000000005";

pub enum AgentPage {
    Inbox(String),
    Remote { machine: String, agent: String },
}

impl AgentPage {
    pub fn from_path(path: &str) -> Option<Self> {
        let parts: Vec<_> = path.strip_prefix('/')?.split('/').collect();
        match parts.as_slice() {
            ["agents", agent, "inbox"] if valid_id(agent) => {
                Some(Self::Inbox(agent.to_ascii_lowercase()))
            }
            ["machines", machine, "agents", agent] if valid_id(machine) && valid_id(agent) => {
                Some(Self::Remote {
                    machine: machine.to_ascii_lowercase(),
                    agent: agent.to_ascii_lowercase(),
                })
            }
            _ => None,
        }
    }
}

// Shared workspace route ownership for Axum and Leptos.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceRoute {
    Setup,
    Routines,
    Workflows,
    Calendar,
    Project(String),
}

impl WorkspaceRoute {
    pub fn is_automation(&self) -> bool {
        matches!(self, Self::Routines | Self::Workflows | Self::Calendar)
    }

    pub fn from_path(path: &str) -> Option<Self> {
        if path == "/" {
            return Some(Self::Setup);
        }
        match path {
            "/routines" => return Some(Self::Routines),
            "/workflows" => return Some(Self::Workflows),
            "/calendar" => return Some(Self::Calendar),
            _ => {}
        }
        let id = path.strip_prefix("/projects/")?;
        valid_id(id).then(|| Self::Project(id.to_ascii_lowercase()))
    }

    pub fn title(&self) -> &'static str {
        match self {
            Self::Setup => "Set up Minarbot",
            Self::Routines => "Routines",
            Self::Workflows => "Workflows",
            Self::Calendar => "Automation calendar",
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
