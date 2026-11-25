use super::*;
use crate::config::WorkspaceConfig;
use rustix::fs::{mkdirat, openat, Mode, OFlags};
use std::{os::fd::OwnedFd, path::PathBuf};

pub fn ensure_managed(state: &AppState, agent: &str) -> Result<PathBuf> {
    if !uuid(agent) {
        return Err(error(400, "Invalid agent ID."));
    }
    managed(state, "agent-workspaces", agent)
}

/// Empty managed folder of a project without folders. Every turn is a
/// harness turn and needs a working directory; this one holds no files.
pub fn ensure_project(state: &AppState, project: &str) -> Result<PathBuf> {
    if !uuid(project) {
        return Err(error(400, "Invalid project ID."));
    }
    managed(state, "project-workspaces", project)
}

fn managed(state: &AppState, group: &str, id: &str) -> Result<PathBuf> {
    let data = state
        .config
        .database
        .parent()
        .ok_or_else(Failure::storage)?;
    let data = std::fs::canonicalize(data).map_err(|_| Failure::storage())?;
    let root = folders::open_directory(&data).map_err(|_| Failure::storage())?;
    let open = |parent: &OwnedFd, name: &str| -> Result<OwnedFd> {
        match mkdirat(parent, name, Mode::from_raw_mode(0o700)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => (),
            Err(_) => return Err(Failure::storage()),
        }
        openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| {
            error(
                409,
                "Managed agent folder is unavailable or contains a symbolic link.",
            )
        })
    };
    let workspaces = open(&root, group)?;
    open(&workspaces, id)?;
    Ok(data.join(group).join(id))
}

fn environment(state: &AppState, agent: &str) -> Option<WorkspaceConfig> {
    state
        .config
        .workspaces
        .iter()
        .find(|w| w.agent == agent)
        .cloned()
}

fn stored(db: &Connection, agent: &str) -> Result<Value> {
    Ok(one(
        db,
        "SELECT directory,write,revision FROM agent_folders WHERE agent=?",
        [agent],
    )?
    .unwrap_or(json!({"revision":0})))
}

fn selected(state: &AppState, row: &Value, agent: &str) -> Result<(&'static str, WorkspaceConfig)> {
    if let Some(binding) = environment(state, agent) {
        return Ok(("server", binding));
    }
    let (source, directory, write) = match row["directory"].as_str() {
        Some(path) => ("custom", PathBuf::from(path), row["write"] == 1),
        None => ("managed", ensure_managed(state, agent)?, true),
    };
    Ok((
        source,
        WorkspaceConfig {
            agent: agent.into(),
            directory,
            write,
            managed: source == "managed",
        },
    ))
}

pub fn effective(state: &AppState, db: &Connection, agent: &str) -> Result<WorkspaceConfig> {
    let row = stored(db, agent)?;
    let (source, binding) = selected(state, &row, agent)?;
    if source == "custom" {
        folders::validate(state, &row["directory"], false, true)?;
    }
    Ok(binding)
}

fn settings(state: &AppState, agent: &str, row: &Value) -> Result<Value> {
    let (source, binding) = selected(state, row, agent)?;
    Ok(json!({
        "path": binding.directory,
        "mode": if binding.write { "read-write" } else { "read-only" },
        "source": source,
        "revision": row["revision"],
    }))
}

pub fn warn_unknown_bindings(config: &crate::config::Config, db: &Connection) -> Result<()> {
    for binding in &config.workspaces {
        if !exists(db, "SELECT 1 FROM agents WHERE id=?", [&binding.agent])? {
            eprintln!(
                "Warning: MINARBOT_WORKSPACES binding references unknown agent {}",
                binding.agent
            );
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn route(
    state: &AppState,
    db: &Connection,
    agent: &str,
    reset: bool,
    get: bool,
    v: &Value,
    q: &[(String, String)],
    headers: &HeaderMap,
) -> Result<Value> {
    if headers.contains_key("authorization")
        || headers.contains_key("origin") && !origin(state, headers)
    {
        return Err(error(403, "Agent folder settings require the local Owner."));
    }
    if !q.is_empty() || !uuid(agent) {
        return Err(error(400, "Invalid agent folder request."));
    }
    agents::read(db, agent)?;
    let row = stored(db, agent)?;
    if get {
        return settings(state, agent, &row);
    }
    if environment(state, agent).is_some() {
        return Err(error(409, "This working folder is set by the server."));
    }
    let keys: &[&str] = if reset {
        &["id", "revision"]
    } else {
        &["id", "revision", "path", "mode"]
    };
    if !closed(v, keys) || !integer(&v["revision"], 0, 9_007_199_254_740_991) {
        return Err(error(400, "Invalid agent folder settings."));
    }
    let id = string(v, "id");
    let request = json!({"agent": agent, "settings": v});
    if let Some(result) = receipt(
        db,
        "agent_folder_commands",
        id,
        &request,
        "ID was already used for another folder change.",
    )? {
        return Ok(result);
    }
    if row["revision"] != v["revision"] {
        return Err(error(409, "Working folder changed. Reload before saving."));
    }
    if exists(
        db,
        "SELECT 1 FROM turns WHERE agent=? AND (status IN('queued','running') OR paused=1) LIMIT 1",
        [agent],
    )? {
        return Err(error(
            409,
            "Finish or cancel agent work before changing its folder.",
        ));
    }
    let (directory, write) = if reset {
        (Value::Null, true)
    } else {
        let write = match string(v, "mode") {
            "read-only" => false,
            "read-write" => true,
            _ => return Err(error(400, "Invalid working folder access mode.")),
        };
        (
            json!(folders::validate(state, &v["path"], false, true)?),
            write,
        )
    };
    let revision = v["revision"].as_i64().ok_or_else(Failure::storage)? + 1;
    db.execute(
        "INSERT INTO agent_folders(agent,directory,write,revision) VALUES(?,?,?,?) ON CONFLICT(agent) DO UPDATE SET directory=excluded.directory,write=excluded.write,revision=excluded.revision",
        rusqlite::params![agent, directory.as_str(), write, revision],
    )?;
    if row["directory"] != directory || (row["write"] != 0) != write {
        agents::reset_harness(db, agent)?;
    }
    let result = settings(
        state,
        agent,
        &json!({"directory":directory,"write":i64::from(write),"revision":revision}),
    )?;
    db.execute(
        "INSERT INTO agent_folder_commands VALUES(?,?,?,?)",
        rusqlite::params![id, agent, request.to_string(), result.to_string()],
    )?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::Path,
        sync::{Arc, Mutex},
    };

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fixture() -> crate::db::Result<(Scratch, AppState, String)> {
        let scratch = Scratch(
            PathBuf::from(std::env::var_os("HOME").ok_or("Missing test home")?)
                .join(format!(".minarbot-folder-test-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&scratch.0)?;
        let config: crate::config::Config =
            serde_json::from_value(json!({"database":scratch.0.join("data/minarbot.sqlite")}))?;
        let database = crate::db::open(&config.database, false, 0)?;
        crate::files::init(&database.connection)?;
        super::super::initialize(&database)?;
        crate::turns::initialize(&database.connection).map_err(|e| e.message)?;
        crate::connectors::initialize(&database.connection).map_err(|e| e.message)?;
        crate::automation::initialize(&database.connection).map_err(|e| e.message)?;
        crate::harness::initialize(&database.connection).map_err(|e| e.message)?;
        crate::mcp::initialize(&database.connection).map_err(|e| e.message)?;
        crate::computer::initialize(&database.connection)?;
        let agent = uuid::Uuid::new_v4().to_string();
        database.connection.execute(
            "INSERT INTO agents(id,name,model,instructions) VALUES(?,'Fixture','','')",
            [&agent],
        )?;
        let state = AppState {
            database: Arc::new(Mutex::new(database)),
            computer: Arc::new(crate::computer::Manager::new(&config)?),
            config: Arc::new(config),
            origin: "http://127.0.0.1".into(),
            auth: None,
            events: crate::events::Hub::new(),
            turns: crate::turns::Runtime::default(),
            whatsapp: Default::default(),
            trial_pending: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        Ok((scratch, state, agent))
    }

    fn checked<T>(result: Result<T>) -> std::result::Result<T, String> {
        result.map_err(|err| err.message)
    }

    fn settings_command(path: &Path, mode: &str, revision: i64) -> Value {
        json!({"id":uuid::Uuid::new_v4().to_string(),"path":path,"mode":mode,"revision":revision})
    }

    #[test]
    fn agent_folder_precedence_and_session_reset() -> crate::db::Result<()> {
        let (scratch, mut state, agent) = fixture()?;
        let custom = scratch.0.join("custom");
        let server = scratch.0.join("server");
        std::fs::create_dir(&custom)?;
        std::fs::create_dir(&server)?;
        let headers = HeaderMap::new();
        {
            let guard = state
                .database
                .lock()
                .map_err(|_| "Database lock poisoned")?;
            let db = &guard.connection;
            let managed = scratch.0.join("data/agent-workspaces").join(&agent);
            assert!(!managed.exists());
            assert_eq!(checked(effective(&state, db, &agent))?.directory, managed);
            assert!(managed.is_dir());
            db.execute(
                "INSERT INTO harness_sessions VALUES(?,'direct','codex',?,'session')",
                rusqlite::params![agent, managed.to_str()],
            )?;
            let command = settings_command(&custom, "read-only", 0);
            let save = || {
                transaction(db, || {
                    route(&state, db, &agent, false, false, &command, &[], &headers)
                })
            };
            let saved = checked(save())?;
            assert_eq!(saved["source"], "custom");
            assert_eq!(checked(save())?, saved);
            let binding = checked(effective(&state, db, &agent))?;
            assert_eq!(binding.directory, custom);
            assert!(!binding.write);
            assert!(!checked(exists(
                db,
                "SELECT 1 FROM harness_sessions WHERE agent=?",
                [&agent]
            ))?);
        }
        Arc::get_mut(&mut state.config)
            .ok_or("Config is shared")?
            .workspaces
            .push(WorkspaceConfig {
                agent: agent.clone(),
                directory: server.clone(),
                write: true,
                managed: false,
            });
        {
            let guard = state
                .database
                .lock()
                .map_err(|_| "Database lock poisoned")?;
            let db = &guard.connection;
            let binding = checked(effective(&state, db, &agent))?;
            assert_eq!(binding.directory, server);
            assert!(binding.write);
            assert_eq!(
                checked(route(
                    &state,
                    db,
                    &agent,
                    false,
                    true,
                    &Value::Null,
                    &[],
                    &headers
                ))?["source"],
                "server"
            );
            let reset = json!({"id":uuid::Uuid::new_v4().to_string(),"revision":1});
            assert!(
                route(&state, db, &agent, true, false, &reset, &[], &headers)
                    .is_err_and(|err| err.status == 409)
            );
        }
        Arc::get_mut(&mut state.config)
            .ok_or("Config is shared")?
            .workspaces
            .clear();
        let guard = state
            .database
            .lock()
            .map_err(|_| "Database lock poisoned")?;
        let db = &guard.connection;
        assert_eq!(checked(effective(&state, db, &agent))?.directory, custom);
        let reset = json!({"id":uuid::Uuid::new_v4().to_string(),"revision":1});
        let restored = checked(transaction(db, || {
            route(&state, db, &agent, true, false, &reset, &[], &headers)
        }))?;
        assert_eq!(restored["source"], "managed");
        assert!(custom.is_dir());
        Ok(())
    }

    #[test]
    fn agent_folder_refuses_invalid_paths_modes_and_remote_edits() -> crate::db::Result<()> {
        let (scratch, state, agent) = fixture()?;
        let symlink = scratch.0.join("link");
        let custom = scratch.0.join("custom");
        std::fs::create_dir(&custom)?;
        std::os::unix::fs::symlink(&custom, &symlink)?;
        let guard = state
            .database
            .lock()
            .map_err(|_| "Database lock poisoned")?;
        let db = &guard.connection;
        let headers = HeaderMap::new();
        for path in [
            PathBuf::from("relative"),
            PathBuf::from("/tmp"),
            scratch.0.join("data"),
            scratch
                .0
                .parent()
                .ok_or("Scratch has no parent")?
                .join(".ssh"),
            scratch.0.join("missing"),
            scratch.0.join("custom/../custom"),
            symlink,
        ] {
            let command = settings_command(&path, "read-only", 0);
            assert!(
                route(&state, db, &agent, false, false, &command, &[], &headers)
                    .is_err_and(|err| err.status == 400),
                "{path:?}"
            );
        }
        let invalid_mode = settings_command(&custom, "execute", 0);
        assert!(route(
            &state,
            db,
            &agent,
            false,
            false,
            &invalid_mode,
            &[],
            &headers
        )
        .is_err_and(|err| err.status == 400));
        let command = settings_command(&custom, "read-only", 0);
        let mut remote = HeaderMap::new();
        remote.insert("authorization", "Bearer test".parse()?);
        assert!(
            route(&state, db, &agent, false, false, &command, &[], &remote)
                .is_err_and(|err| err.status == 403)
        );
        assert_eq!(checked(stored(db, &agent))?["revision"], 0);
        Ok(())
    }
}
