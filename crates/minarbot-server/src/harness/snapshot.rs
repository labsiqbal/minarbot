use super::*;
use crate::data::transaction;
use std::collections::BTreeSet;

use crate::mcp::secrets::redact;

pub struct Snapshot {
    state: AppState,
    call: i64,
    turn: String,
    segments: Vec<Value>,
    dirty: BTreeSet<usize>,
    revision: i64,
    last: Instant,
    secrets: Vec<String>,
    closed: bool,
    private: bool,
}
impl Snapshot {
    pub fn new(state: &AppState, call: i64, turn: &str, secrets: Vec<String>) -> Self {
        Self {
            state: state.clone(),
            call,
            turn: turn.into(),
            segments: Vec::new(),
            dirty: BTreeSet::new(),
            revision: 0,
            last: Instant::now() - Duration::from_secs(1),
            secrets,
            closed: false,
            private: false,
        }
    }
    pub fn private(mut self, private: bool) -> Self {
        self.private = private;
        self
    }
    pub fn current(&self) -> &str {
        self.segments
            .last()
            .filter(|s| s["kind"] == "text")
            .map(|s| string(s, "text"))
            .unwrap_or("")
    }
    pub fn text(&mut self, value: &str, id: Option<&str>, replace: bool) -> Result<()> {
        let index = if let Some(index) =
            id.and_then(|id| self.segments.iter().position(|s| s["id"] == id))
        {
            if self.segments[index]["kind"] != "text" {
                return Err(error(502, "Invalid harness text identity."));
            }
            index
        } else if self
            .segments
            .last()
            .is_some_and(|s| s["kind"] == "text" && id.is_none_or(|id| s["id"] == id))
        {
            self.segments.len() - 1
        } else {
            if self.segments.len() >= 512 || id.is_some_and(|id| id.len() > 300) {
                return Err(error(502, "Coding harness stream has too many segments."));
            }
            self.segments.push(json!({"id":id.map(str::to_owned).unwrap_or_else(||format!("text-{}",self.segments.len())),"kind":"text","text":"","state":"","final":0}));
            self.segments.len() - 1
        };
        let next = if replace {
            value.to_owned()
        } else {
            format!("{}{}", string(&self.segments[index], "text"), value)
        };
        if next == self.segments[index]["text"] {
            return Ok(());
        }
        if self
            .segments
            .iter()
            .map(|s| string(s, "text").len())
            .sum::<usize>()
            - string(&self.segments[index], "text").len()
            + next.len()
            > 2 * 1024 * 1024
        {
            return Err(error(502, "Coding harness answer exceeded 2 MiB."));
        }
        self.segments[index]["text"] = json!(next);
        self.dirty.insert(index);
        if self.last.elapsed() >= Duration::from_millis(40) {
            self.flush()?;
        }
        Ok(())
    }
    pub fn tool(
        &mut self,
        id: &str,
        name: &str,
        state: &str,
        arguments: &Value,
        result: &Value,
    ) -> Result<()> {
        if self.private {
            return Err(error(502, "Goal judge attempted to use a tool."));
        }
        if id.is_empty() || id.len() > 300 || name.len() > 2000 || state.len() > 100 {
            return Err(error(502, "Invalid harness tool identity."));
        }
        let failed = crate::turns::diagnostics::tool_failed(result);
        let key = format!("tool-{id}");
        let existing = self
            .segments
            .iter()
            .position(|segment| segment["id"] == key);
        let previous = existing.map(|index| &self.segments[index]);
        let name = crate::turns::diagnostics::text(
            if name.is_empty() {
                previous
                    .and_then(|segment| segment["text"].as_str())
                    .unwrap_or("Tool")
            } else {
                name
            },
            &self.secrets,
        );
        let private =
            crate::connectors::mailbox::is_tool(name.rsplit("__").next().unwrap_or(&name));
        let arguments = crate::turns::diagnostics::display(
            if private { &Value::Null } else { arguments },
            &self.secrets,
        );
        let result = crate::turns::diagnostics::display(
            if private { &Value::Null } else { result },
            &self.secrets,
        );
        let state = if state.is_empty() {
            previous
                .and_then(|segment| segment["state"].as_str())
                .unwrap_or("pending")
                .to_owned()
        } else {
            state.to_owned()
        };
        let terminal = matches!(
            state.as_str(),
            "completed"
                | "done"
                | "failed"
                | "declined"
                | "denied"
                | "cancelled"
                | "interrupted"
                | "error"
                | "aborted"
        );
        self.flush()?;
        let index = if let Some(index) = existing {
            index
        } else {
            if self.segments.len() >= 512 {
                return Err(error(502, "Coding harness stream has too many segments."));
            }
            self.segments
                .push(json!({"id":key,"kind":"tool","text":name,"state":state,"final":0}));
            self.segments.len() - 1
        };
        {
            let guard = self.state.database.lock().map_err(|_| Failure::storage())?;
            let db = &guard.connection;
            let now = crate::data::iso(db, self.state.config.now())?;
            transaction(db, || {
                db.execute(
                    "INSERT INTO harness_steps(turn,id,name,state) VALUES(?,?,?,?)
                     ON CONFLICT(turn,id) DO UPDATE SET name=excluded.name,state=excluded.state",
                    rusqlite::params![self.turn, id, name, state],
                )?;
                db.execute(
                    "INSERT INTO turn_step_diagnostics(turn,id,started,finished,arguments,result,failed) VALUES(?,?,?,?,?,?,?)
                     ON CONFLICT(turn,id) DO UPDATE SET started=coalesce(started,excluded.started),
                     finished=coalesce(excluded.finished,finished),arguments=coalesce(excluded.arguments,arguments),
                     result=coalesce(excluded.result,result),failed=max(failed,excluded.failed)",
                    rusqlite::params![self.turn, id, (!terminal).then_some(&now), terminal.then_some(&now), arguments, result, failed],
                )?;
                Ok(())
            })?;
        }
        self.segments[index]["text"] = json!(name);
        self.segments[index]["state"] = json!(state);
        self.dirty.insert(index);
        self.flush()
    }
    pub fn flush(&mut self) -> Result<()> {
        if self.private || self.dirty.is_empty() {
            return Ok(());
        }
        self.revision += 1;
        let guard = self.state.database.lock().map_err(|_| Failure::storage())?;
        let db = &guard.connection;
        transaction(db, || {
            for index in &self.dirty {
                let s = &self.segments[*index];
                db.execute("INSERT INTO provider_stream_segments VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(call,ordinal) DO UPDATE SET text=excluded.text,state=excluded.state,final=excluded.final,revision=excluded.revision",rusqlite::params![self.call,*index as i64,string(s,"id"),string(s,"kind"),redact(string(s,"text"),&self.secrets,s["final"]!=1),string(s,"state"),s["final"].as_i64(),self.revision])?;
            }
            db.execute("INSERT INTO provider_streams VALUES(?,?) ON CONFLICT(call) DO UPDATE SET text=excluded.text",rusqlite::params![self.call,redact(self.current(),&self.secrets,self.segments.last().is_none_or(|s|s["final"]!=1))])?;
            Ok(())
        })?;
        self.dirty.clear();
        self.last = Instant::now();
        self.state.events.turn(db, &self.turn)?;
        Ok(())
    }
    pub fn finish(mut self) -> Result<String> {
        let text = self.current().to_owned();
        if text.trim().is_empty() {
            return Err(error(502,"Coding harness returned no complete text answer. Check workspace changes before retrying."));
        }
        if let Some(last) = self.segments.last_mut() {
            last["final"] = json!(1);
            self.dirty.insert(self.segments.len() - 1);
        }
        self.flush()?;
        self.closed = true;
        Ok(redact(&text, &self.secrets, false))
    }
}
impl Drop for Snapshot {
    fn drop(&mut self) {
        if !self.closed {
            let _ = self.flush();
            if let Ok(db) = self.state.database.lock() {
                let _=db.connection.execute("UPDATE provider_calls SET status='failed',finished=? WHERE seq=? AND status='pending'",rusqlite::params![data::iso(&db.connection,self.state.config.now()).ok(),self.call]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_coalesces_writes_and_flushes_terminal_tails_without_subscribers(
    ) -> crate::db::Result<()> {
        use std::sync::{Arc, Mutex};

        let scratch = process::ProbeHome::new()?;
        let config: crate::config::Config =
            serde_json::from_value(json!({"database":scratch.0.join("state.sqlite")}))?;
        let database = crate::db::open(&config.database, false, 0)?;
        crate::turns::initialize_canonical(&database.connection).map_err(|e| e.message)?;
        database.connection.execute_batch("INSERT INTO agents(id,name,model,instructions) VALUES('agent','Test','','');
            INSERT INTO turns(id,agent,text,status,created) VALUES('turn','agent','','running','');
            INSERT INTO provider_calls(seq,turn,agent,model,origin,purpose,status,created)
                VALUES (1,'turn','agent','','','completion','pending',''), (2,'turn','agent','','','completion','pending','');
            CREATE TABLE room_turns (turn TEXT);
            CREATE TABLE writes (n INTEGER);
            CREATE TRIGGER stream_insert AFTER INSERT ON provider_streams BEGIN INSERT INTO writes VALUES (1); END;
            CREATE TRIGGER stream_update AFTER UPDATE ON provider_streams BEGIN INSERT INTO writes VALUES (1); END;")?;
        let state = AppState {
            computer: Arc::new(crate::computer::Manager::new(&config)?),
            config: Arc::new(config),
            database: Arc::new(Mutex::new(database)),
            origin: "http://127.0.0.1".to_owned(),
            auth: None,
            events: crate::events::Hub::new(),
            turns: crate::turns::Runtime::default(),
            whatsapp: Default::default(),
            trial_pending: Arc::default(),
        };
        let read = |call: i64| -> crate::db::Result<(i64, String)> {
            let db = state
                .database
                .lock()
                .map_err(|_| "database lock poisoned")?;
            Ok(db.connection.query_row(
                "SELECT (SELECT count(*) FROM writes), text FROM provider_streams WHERE call=?",
                [call],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        };
        let mut writer = Snapshot::new(&state, 1, "turn", Vec::new());
        writer.text("A", None, false).map_err(|e| e.message)?;
        writer.last = Instant::now() + Duration::from_secs(60);
        for _ in 0..199 {
            writer.text("x", None, false).map_err(|e| e.message)?;
        }
        assert_eq!(read(1)?, (1, "A".to_owned()));
        writer.last = Instant::now() - Duration::from_millis(40);
        writer.text("x", None, false).map_err(|e| e.message)?;
        assert_eq!(read(1)?, (2, format!("A{}", "x".repeat(200))));
        writer
            .tool("tool", "Read", "running", &Value::Null, &Value::Null)
            .map_err(|e| e.message)?;
        writer.last = Instant::now() + Duration::from_secs(60);
        writer.text("Draft", None, false).map_err(|e| e.message)?;
        assert_eq!(read(1)?, (3, String::new()));
        writer.text("OK", None, true).map_err(|e| e.message)?;
        assert_eq!(writer.finish().map_err(|e| e.message)?, "OK");
        assert_eq!(read(1)?, (4, "OK".to_owned()));

        let mut interrupted = Snapshot::new(&state, 2, "turn", Vec::new());
        interrupted
            .text("First", None, false)
            .map_err(|e| e.message)?;
        interrupted.last = Instant::now() + Duration::from_secs(60);
        interrupted
            .text(" buffered tail", None, false)
            .map_err(|e| e.message)?;
        assert_eq!(read(2)?, (5, "First".to_owned()));
        drop(interrupted);
        assert_eq!(read(2)?, (6, "First buffered tail".to_owned()));

        let db = state
            .database
            .lock()
            .map_err(|_| "database lock poisoned")?;
        let segments = all(&db.connection, "SELECT call,ordinal,kind,text,final FROM provider_stream_segments ORDER BY call,ordinal", []).map_err(|e| e.message)?;
        assert_eq!(
            json!(segments),
            json!([
                {"call":1,"ordinal":0,"kind":"text","text":format!("A{}", "x".repeat(200)),"final":0},
                {"call":1,"ordinal":1,"kind":"tool","text":"Read","final":0},
                {"call":1,"ordinal":2,"kind":"text","text":"OK","final":1},
                {"call":2,"ordinal":0,"kind":"text","text":"First buffered tail","final":0}
            ])
        );
        Ok(())
    }

    #[test]
    fn redact_masks_complete_escaped_and_partial_streaming_values() {
        let secret = "PROJECT_CANARY_j4W8sK3p".to_owned();
        let secrets = [secret.clone()];
        assert_eq!(
            redact(&format!("prefix {secret}"), &secrets, true),
            "prefix [REDACTED]"
        );
        assert_eq!(
            redact(&format!("prefix {}", &secret[..10]), &secrets, true),
            "prefix "
        );
        assert_eq!(
            redact(
                r#"{"value":"line\nsecret"}"#,
                &["line\nsecret".to_owned()],
                false
            ),
            r#"{"value":"[REDACTED]"}"#
        );
    }
}
