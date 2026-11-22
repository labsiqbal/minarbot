use super::*;
use crate::db::Database;
fn column(db: &Connection, table: &str, name: &str, definition: &str) -> crate::db::Result<()> {
    let mut q = db.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = q
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if !names.iter().any(|v| v == name) {
        db.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {name} {definition}"
        ))?;
    }
    Ok(())
}
pub fn initialize(database: &Database) -> crate::db::Result<()> {
    initialize_mode(database, false)
}
pub fn initialize_canonical(database: &Database) -> crate::db::Result<()> {
    initialize_mode(database, true)
}
fn initialize_mode(database: &Database, canonical: bool) -> crate::db::Result<()> {
    let db = &database.connection;
    deletion::initialize(db)?;
    db.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> crate::db::Result<()> {
        db.execute_batch("CREATE TABLE IF NOT EXISTS channel_members(channel TEXT NOT NULL REFERENCES channels(id),agent TEXT NOT NULL REFERENCES agents(id),PRIMARY KEY(channel,agent)) STRICT;")?;
        let legacy_activity = !all(db, "PRAGMA table_info(channels)", [])
            .map_err(|_| "Cannot read channel columns")?
            .iter()
            .any(|column| column["name"] == "activity");
        for (table, cols) in [
            (
                "channels",
                vec![
                    ("revision", "INTEGER NOT NULL DEFAULT 1"),
                    ("archived", "INTEGER NOT NULL DEFAULT 0"),
                    ("activity", "INTEGER NOT NULL DEFAULT 0"),
                    ("read_position", "INTEGER NOT NULL DEFAULT 0"),
                ],
            ),
            (
                "turns",
                vec![
                    ("recipient", "TEXT REFERENCES agents(id)"),
                    ("automatic", "INTEGER NOT NULL DEFAULT 0"),
                    ("routing", "TEXT"),
                    ("paused", "INTEGER NOT NULL DEFAULT 0"),
                    ("revision", "INTEGER NOT NULL DEFAULT 1"),
                    ("no_update", "INTEGER NOT NULL DEFAULT 0"),
                    ("waiting_for", "TEXT REFERENCES turns(id)"),
                    ("origin", "TEXT"),
                    ("message_depth", "INTEGER NOT NULL DEFAULT 0"),
                    ("started_at", "TEXT"),
                    ("stopped_at", "TEXT"),
                    ("dispatch_mode", "TEXT NOT NULL DEFAULT 'queue'"),
                    ("dispatch_target", "TEXT"),
                    ("dispatch_label", "TEXT"),
                ],
            ),
        ] {
            for (name, definition) in cols {
                column(db, table, name, definition)?;
            }
        }
        db.execute_batch(include_str!("schema.sql"))?;
        if !canonical {
            db.execute_batch(include_str!("local.sql"))?;
            device_pairing::initialize(db)?;
        }
        environment::initialize(db)?;
        for (table, name, definition) in [
            (
                "agents",
                "web_access",
                "INTEGER NOT NULL DEFAULT 1 CHECK(web_access IN(0,1))",
            ),
            ("local_projects", "archived", "INTEGER NOT NULL DEFAULT 0"),
            (
                "local_projects",
                "codegraph_enabled",
                "INTEGER NOT NULL DEFAULT 0 CHECK(codegraph_enabled IN(0,1))",
            ),
            (
                "local_projects",
                "allow_installation_apps",
                "INTEGER NOT NULL DEFAULT 0",
            ),
            ("channels", "project", "TEXT REFERENCES local_projects(id)"),
            (
                "memories",
                "selection_locked",
                "INTEGER NOT NULL DEFAULT 1 CHECK(selection_locked IN(0,1))",
            ),
            ("room_discussions", "stopped", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            column(db, table, name, definition)?;
        }
        db.execute_batch("CREATE TABLE IF NOT EXISTS agent_creations(id TEXT PRIMARY KEY REFERENCES agents(id),request TEXT NOT NULL) STRICT;
            CREATE TABLE IF NOT EXISTS agent_edits(id TEXT PRIMARY KEY,agent TEXT NOT NULL REFERENCES agents(id),request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
            CREATE TABLE IF NOT EXISTS thread_commands(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
            CREATE TABLE IF NOT EXISTS channel_creations(id TEXT PRIMARY KEY REFERENCES channels(id),request TEXT NOT NULL) STRICT;
            CREATE TABLE IF NOT EXISTS channel_edits(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL) STRICT;
            CREATE TABLE IF NOT EXISTS deleted_agents(id TEXT PRIMARY KEY) STRICT;
            CREATE TABLE IF NOT EXISTS local_project_sections(project TEXT NOT NULL REFERENCES local_projects(id),channel TEXT PRIMARY KEY REFERENCES channels(id),name TEXT NOT NULL,position INTEGER NOT NULL,archived INTEGER NOT NULL DEFAULT 0,revision INTEGER NOT NULL DEFAULT 1,UNIQUE(project,name)) STRICT;
            CREATE TABLE IF NOT EXISTS local_project_legacy_channels(channel TEXT PRIMARY KEY REFERENCES channels(id)) STRICT;
            CREATE TRIGGER IF NOT EXISTS channel_turn_created AFTER INSERT ON turns WHEN NEW.channel IS NOT NULL BEGIN UPDATE channels SET activity=activity+1 WHERE id=NEW.channel; END;
            CREATE TRIGGER IF NOT EXISTS channel_turn_changed AFTER UPDATE ON turns WHEN NEW.channel IS NOT NULL AND(NEW.status IS NOT OLD.status OR NEW.reply IS NOT OLD.reply OR NEW.agent IS NOT OLD.agent OR NEW.revision IS NOT OLD.revision) BEGIN UPDATE channels SET activity=activity+1 WHERE id=NEW.channel; END;
            ")?;
        memory::initialize(db, canonical)?;
        if canonical {
            return Ok(());
        }
        column(
            db,
            "agents",
            "learn_from_conversations",
            "INTEGER NOT NULL DEFAULT 1 CHECK(learn_from_conversations IN(0,1))",
        )?;
        column(db, "agents", "memory_model", "TEXT NOT NULL DEFAULT ''")?;
        column(db, "turns", "attention_finished_at", "INTEGER")?;
        db.execute_batch("UPDATE turns SET status='interrupted',error='The process stopped before the result was saved. Check the provider before sending again.' WHERE status='running'; UPDATE provider_calls SET status='interrupted' WHERE status='pending';")?;
        for c in all(
            db,
            "SELECT * FROM channels WHERE id NOT IN(SELECT id FROM channel_creations)",
            [],
        )
        .map_err(|_| "Cannot read channel creations")?
        {
            let members = all(
                db,
                "SELECT agent FROM channel_members WHERE channel=? ORDER BY agent",
                [string(&c, "id")],
            )
            .map_err(|_| "Cannot read channel members")?
            .into_iter()
            .map(|member| member["agent"].clone())
            .collect::<Vec<_>>();
            db.execute("INSERT INTO channel_creations VALUES(?,?)", rusqlite::params![string(&c, "id"),
                json!({"name":c["name"],"instructions":c["instructions"],"members":members,"lead":c["lead"]}).to_string()])?;
        }
        if legacy_activity {
            db.execute_batch("UPDATE channels SET activity=(SELECT count(*) FROM turns WHERE channel=channels.id)")?;
        }
        projects::migrate(db, crate::config::now()).map_err(|_| "Project migration failed")?;
        delivery::migrate(db, crate::config::now()).map_err(|_| "Conversation migration failed")?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            db.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(e) => {
            db.execute_batch("ROLLBACK")?;
            Err(e)
        }
    }
}
