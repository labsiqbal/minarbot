use crate::{db::Database, web::Failure};
use axum::{
    body::Body,
    http::{header, HeaderValue},
    response::Response,
};
use rusqlite::OptionalExtension;
use serde_json::json;
use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{broadcast, watch};

pub struct Scope {
    table: &'static str,
    clause: &'static str,
    id: String,
}
pub fn scope(path: &str) -> Option<Scope> {
    let parts: Vec<_> = path.split('/').collect();
    let ["", "api", kind, id, "turns", "stream"] = parts.as_slice() else {
        return None;
    };
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) || byte == b'-')
    {
        return None;
    }
    let (table, clause) = match *kind {
        "agents" => ("agents", "agent=? AND channel IS NULL AND thread IS NULL"),
        "threads" => ("threads", "thread=? AND channel IS NULL"),
        "channels" => ("channels", "channel=?"),
        _ => return None,
    };
    Some(Scope {
        table,
        clause,
        id: id.to_string(),
    })
}
fn exists(db: &Database, scope: &Scope) -> Result<bool, Failure> {
    let predicate = if scope.table == "threads" {
        " AND deleted=0"
    } else {
        ""
    };
    Ok(db
        .connection
        .query_row(
            &format!("SELECT 1 FROM {} WHERE id=?{predicate}", scope.table),
            [&scope.id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}
fn snapshots(db: &Database, scope: &Scope) -> Result<Vec<(String, String)>, Failure> {
    let sql = format!(
        "SELECT id FROM turns WHERE {} AND status='running' ORDER BY seq LIMIT 100",
        scope.clause
    );
    let mut statement = db.connection.prepare(&sql)?;
    let turns = statement
        .query_map([&scope.id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut frames = Vec::new();
    for turn in turns {
        let partial = crate::data::partial(&db.connection, &turn)?;
        if !partial.is_null() {
            frames.push((
                turn.clone(),
                format!("data: {}\n\n", json!({"id":turn,"partial":partial})),
            ));
        }
    }
    Ok(frames)
}
#[derive(Clone)]
pub struct Hub {
    shutdown: watch::Sender<bool>,
    changes: broadcast::Sender<String>,
}
impl Hub {
    pub fn new() -> Self {
        let (shutdown, _) = watch::channel(false);
        let (changes, _) = broadcast::channel(256);
        Self { shutdown, changes }
    }
    pub fn watch(&self, db: &rusqlite::Connection) {
        let hub = self.clone();
        db.commit_hook(Some(move || {
            hub.publish("database".to_owned());
            false
        }));
    }
    pub fn publish(&self, scope: String) {
        let _ = self.changes.send(scope);
    }
    pub fn turn(&self, db: &rusqlite::Connection, id: &str) -> Result<(), Failure> {
        let Some(mut turn) = crate::data::one(
            db,
            "SELECT id,agent,thread,channel FROM turns WHERE id=? AND status='running'",
            [id],
        )?
        else {
            return Ok(());
        };
        let partial = crate::data::partial(db, id)?;
        if !partial.is_null() {
            turn["partial"] = partial;
            self.publish(turn.to_string());
        }
        Ok(())
    }
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.changes.subscribe()
    }
    pub fn close(&self) {
        self.shutdown.send_replace(true);
    }
    pub fn calendar(&self) -> Response {
        let mut changes = self.changes.subscribe();
        let mut shutdown = self.shutdown.subscribe();
        let stream = async_stream::stream! {
            yield Ok::<_, Infallible>("data: changed\n\n");
            let mut heartbeat = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_secs(15), Duration::from_secs(15));
            loop {
                tokio::select! {
                    _ = shutdown.changed() => break,
                    _ = heartbeat.tick() => { yield Ok(": keepalive\n\n"); continue; },
                    change = changes.recv() => match change {
                        Ok(scope) if scope == "database" => (),
                        Err(broadcast::error::RecvError::Lagged(_)) => (),
                        Err(broadcast::error::RecvError::Closed) => break,
                        _ => continue,
                    }
                }
                tokio::time::sleep(Duration::from_millis(150)).await;
                while changes.try_recv().is_ok() {}
                yield Ok("data: changed\n\n");
            }
        };
        stream_response(Body::from_stream(stream))
    }
    pub fn sidebar(&self, db: &Arc<Mutex<Database>>) -> Response {
        let mut changes = self.changes.subscribe();
        let mut shutdown = self.shutdown.subscribe();
        let db = db.clone();
        let stream = async_stream::stream! {
            let mut previous = String::new();
            let mut refresh = true;
            let mut heartbeat = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_secs(15), Duration::from_secs(15));
            loop {
                if refresh {
                    let snapshot = db.lock().map_err(|_| Failure::storage()).and_then(|guard| crate::data::sidebar::attention(&guard.connection));
                    let Ok(snapshot) = snapshot else { break };
                    let current = snapshot.to_string();
                    if current != previous {
                        yield Ok::<_, Infallible>(format!("data: {current}\n\n"));
                        previous = current;
                    }
                }
                refresh = tokio::select! {
                    _ = shutdown.changed() => break,
                    _ = heartbeat.tick() => {
                        yield Ok(": keepalive\n\n".to_owned());
                        false
                    }
                    change = changes.recv() => match change {
                        Ok(scope) => scope == "database",
                        Err(broadcast::error::RecvError::Lagged(_)) => true,
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                };
                if refresh {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    while changes.try_recv().is_ok() {}
                }
            }
        };
        stream_response(Body::from_stream(stream))
    }
    pub fn response(&self, scope: Scope, db: &Arc<Mutex<Database>>) -> Result<Response, Failure> {
        {
            let guard = db.lock().map_err(|_| Failure::storage())?;
            if !exists(&guard, &scope)? {
                return Err(Failure::new(
                    404,
                    "resource_not_found",
                    "Conversation not found.",
                ));
            }
        }
        let mut changes = self.changes.subscribe();
        let db = db.clone();
        let mut shutdown = self.shutdown.subscribe();
        let stream = async_stream::stream! {
            let mut previous = HashMap::new();
            let mut initial = true;
            let mut heartbeat = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_secs(15), Duration::from_secs(15));
            loop {
                tokio::select! {
                    _ = shutdown.changed() => break,
                    _ = heartbeat.tick() => yield Ok::<_, Infallible>(": keepalive\n\n".to_owned()),
                    event = async { if initial { initial = false; None } else { changes.recv().await.ok() } } => {
                        let result = db.lock().map_err(|_| Failure::storage()).and_then(|db| if exists(&db, &scope)? { snapshots(&db, &scope).map(Some) } else { Ok(None) });
                        let current = match result { Ok(Some(frames)) => frames, _ => break };
                        let mut events=event.into_iter().collect::<Vec<_>>();
                        while let Ok(event)=changes.try_recv() {events.push(event);}
                        let mut frames=Vec::new();
                        for event in events {
                            if let Ok(turn)=serde_json::from_str::<serde_json::Value>(&event) {
                                let visible=match scope.table {
                                    "channels"=>turn["channel"]==scope.id,
                                    "threads"=>turn["thread"]==scope.id && turn["channel"].is_null(),
                                    _=>turn["agent"]==scope.id && turn["thread"].is_null() && turn["channel"].is_null(),
                                };
                                if visible {
                                    if let Some(id)=turn["id"].as_str() {frames.push((id.to_owned(),format!("data: {}\n\n",json!({"id":id,"partial":turn["partial"]}))));}
                                }
                            }
                        }
                        frames.extend(current);
                        for (turn, frame) in frames { if previous.get(&turn) != Some(&frame) { previous.insert(turn, frame.clone()); yield Ok(frame); } }
                        if previous.len()>100 {previous.clear();}
                    }
                }
            }
        };
        Ok(stream_response(Body::from_stream(stream)))
    }
}

fn stream_response(body: Body) -> Response {
    let mut response = Response::new(body);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn committed_writes_broadcast_without_polling() -> Result<(), Box<dyn std::error::Error>> {
        let db = rusqlite::Connection::open_in_memory()?;
        db.execute_batch("CREATE TABLE records(id INTEGER)")?;
        let hub = Hub::new();
        hub.watch(&db);
        let mut listener = hub.changes.subscribe();
        db.execute_batch("BEGIN; INSERT INTO records VALUES(1); ROLLBACK")?;
        assert!(matches!(
            listener.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        db.execute("INSERT INTO records VALUES(2)", [])?;
        assert_eq!(listener.try_recv()?, "database");
        Ok(())
    }
}
