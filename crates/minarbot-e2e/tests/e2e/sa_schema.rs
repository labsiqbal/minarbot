//! Fresh and upgraded startup schemas match the frozen 85373b3 snapshot plus
//! the Rust extensions (port of `app/schema.test.ts`; the hosted hub case is
//! the unit test `hub::schema_tests` in `minarbot-server`).

use minarbot_testkit::{
    canonical::Canonical,
    db::{Fixture, Inspector},
    prelude::*,
    TestRoot,
};

const CANONICAL: &str =
    include_str!("../../../minarbot-testkit/fixtures/canonical-pre-collapse.sql");
const LOCAL: &str = include_str!("../../../minarbot-testkit/fixtures/local-pre-collapse.sql");
const PROVIDER_INSTANCES: &str =
    include_str!("../../../minarbot-server/src/data/provider-instances.sql");
const GOALS: &str = include_str!("../../../minarbot-server/src/turns/goals.sql");
const FILES: &str = include_str!("../../../minarbot-server/src/files/schema.sql");
const LIBRARY_DOCS: &str =
    include_str!("../../../minarbot-server/src/turns/library_docs/schema.sql");
const MEMORY_SCHEMA: &str = include_str!("../../../minarbot-memory/src/schema.sql");
const MEMORY_RETAIN: &str = include_str!("../../../minarbot-memory/src/retain.sql");
const MEMORY_REFLECT: &str = include_str!("../../../minarbot-memory/src/reflect.sql");
const MEMORY_HISTORY_CANONICAL: &str =
    include_str!("../../../minarbot-server/src/data/memory-history-canonical.sql");
const MEMORY_IMPORT: &str = include_str!("../../../minarbot-server/src/data/memory-import.sql");
const MEMORY_WORKER: &str = include_str!("../../../minarbot-server/src/data/memory-worker.sql");

const RETAINED: &str = "SELECT a.*, c.request, t.reply, t.status, p.enabled, o.agent AS always_on
  FROM agents a JOIN agent_creations c ON c.id=a.id JOIN turns t ON t.agent=a.id
  JOIN agent_computers p ON p.agent=a.id JOIN computer_always_on o ON o.agent=a.id
  WHERE a.id='fixture-agent'";

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Canonical,
    Local,
}

/// Schema objects (whitespace-normalized SQL; `agents` SQL and the provider
/// instance table left out, as in the Node snapshot) and the application id.
fn snapshot(rows: Vec<Value>, application_id: Value) -> Value {
    let objects: Vec<Value> = rows
        .into_iter()
        .filter(|row| {
            row["name"] != "provider_instances" && row["name"] != "sqlite_autoindex_provider_instances_1"
        })
        .map(|row| {
            let sql = if row["name"] == "agents" {
                Value::Null
            } else {
                match row["sql"].as_str() {
                    Some(sql) => json!(sql.split_whitespace().collect::<Vec<_>>().join(" ")),
                    None => Value::Null,
                }
            };
            json!({"type": row["type"], "name": row["name"], "tbl_name": row["tbl_name"], "sql": sql})
        })
        .collect();
    json!({"objects": objects, "userVersion": 0, "applicationId": application_id})
}

const OBJECTS: &str = "SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name";

fn fixture_snapshot(db: &Fixture) -> Result<Value> {
    Ok(snapshot(
        db.rows(OBJECTS, [])?,
        json!(db.scalar::<i64>("PRAGMA application_id", [])?),
    ))
}

fn inspector_snapshot(db: &Inspector) -> Result<Value> {
    Ok(snapshot(
        db.rows(OBJECTS, [])?,
        json!(db.scalar::<i64>("PRAGMA application_id", [])?),
    ))
}

fn add_memory_schema(db: &Fixture, mode: Mode, phase2: bool) -> Result<()> {
    db.execute_batch(MEMORY_SCHEMA)?;
    if phase2 {
        db.execute_batch(MEMORY_RETAIN)?;
        db.execute_batch(MEMORY_REFLECT)?;
    }
    if mode == Mode::Canonical {
        db.execute_batch(MEMORY_HISTORY_CANONICAL)?;
    } else {
        db.execute_batch(
            "CREATE VIEW memory_history_scope AS
              SELECT t.id, CASE WHEN t.channel IS NOT NULL THEN 'channels:'||t.channel
                                ELSE 'agents:'||coalesce(th.agent,t.agent) END AS bank
              FROM turns t LEFT JOIN threads th ON th.id=t.thread
              WHERE t.channel IS NOT NULL OR coalesce(th.agent,t.agent) IS NOT NULL",
        )?;
    }
    db.execute_batch(MEMORY_IMPORT)?;
    db.execute_batch("CREATE TABLE memory_imports(version INTEGER PRIMARY KEY) STRICT")?;
    if mode == Mode::Local && phase2 {
        db.execute_batch(
            "ALTER TABLE agents ADD COLUMN learn_from_conversations INTEGER NOT NULL DEFAULT 1 CHECK(learn_from_conversations IN(0,1)); \
             ALTER TABLE agents ADD COLUMN memory_model TEXT NOT NULL DEFAULT '';",
        )?;
        db.execute_batch(MEMORY_WORKER)?;
    }
    Ok(())
}

fn config(mode: Mode) -> ServerConfig {
    match mode {
        Mode::Canonical => ServerConfig::new().option(
            "auth",
            json!({"secretKey": "sk_test_fixture", "publishableKey": "pk_test_fixture",
                "issuer": "https://fixture.clerk.accounts.dev", "origin": "https://minarbot.example",
                "development": true}),
        ),
        Mode::Local => ServerConfig::new(),
    }
}

async fn start_and_close(root: &TestRoot, mode: Mode) -> Result<()> {
    let mut app = root.start(config(mode)).await?;
    app.shutdown().await?;
    Ok(())
}

/// Frozen snapshot (85373b3 plus #87 provider columns) and the snapshot the
/// current server must produce.
fn expected(mode: Mode) -> Result<(Value, Value)> {
    let db = Fixture::open(":memory:")?;
    db.execute_batch(if mode == Mode::Canonical {
        CANONICAL
    } else {
        LOCAL
    })?;
    db.execute_batch(PROVIDER_INSTANCES)?;
    db.execute_batch(
        "ALTER TABLE agents ADD COLUMN provider_instance TEXT NOT NULL DEFAULT ''; \
         ALTER TABLE agents ADD COLUMN model_options TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(model_options));",
    )?;
    let frozen = fixture_snapshot(&db)?;
    if mode == Mode::Local {
        db.execute_batch(GOALS)?;
        db.execute_batch(
            "CREATE TABLE craft_deep(project TEXT PRIMARY KEY REFERENCES local_projects(id) ON DELETE CASCADE) STRICT;
             CREATE TABLE install_token_targets(
               token BLOB PRIMARY KEY REFERENCES install_tokens(digest) ON DELETE CASCADE,
               origin TEXT NOT NULL
             ) STRICT;",
        )?;
    }
    db.execute_batch(
        "CREATE TABLE pairing_code_targets(id TEXT PRIMARY KEY REFERENCES pairing_codes(id) ON DELETE CASCADE,origin TEXT NOT NULL) STRICT;
         CREATE TABLE mcp_apps(id TEXT PRIMARY KEY,turn TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,connection TEXT NOT NULL REFERENCES mcp_connections(id) ON DELETE CASCADE,revision INTEGER NOT NULL,title TEXT NOT NULL,html TEXT NOT NULL,bootstrap TEXT NOT NULL) STRICT;
         CREATE INDEX mcp_apps_turn ON mcp_apps(turn);
         CREATE TABLE mcp_app_approvals(app TEXT PRIMARY KEY REFERENCES mcp_apps(id) ON DELETE CASCADE,id TEXT NOT NULL,digest TEXT NOT NULL,expires INTEGER NOT NULL) STRICT;",
    )?;
    db.execute_batch(FILES)?;
    db.execute_batch(
        "ALTER TABLE agents ADD COLUMN library_docs_access INTEGER NOT NULL DEFAULT 1 CHECK(library_docs_access IN(0,1))",
    )?;
    db.execute_batch(LIBRARY_DOCS)?;
    add_memory_schema(&db, mode, true)?;
    Ok((frozen, fixture_snapshot(&db)?))
}

/// `snapshot` without the objects of `tables` (tables and their autoindexes).
fn without(snapshot: &Value, tables: &[&str]) -> Value {
    let mut snapshot = snapshot.clone();
    if let Some(objects) = snapshot["objects"].as_array_mut() {
        objects.retain(|object| !tables.iter().any(|table| object["tbl_name"] == *table));
    }
    snapshot
}

/// Legacy provider storage #87 removed: upgrades drop these tables (and the
/// `agents` columns `runtime`, `provider_account`, `backup_model`).
const DROPPED_BY_UPGRADE: &[&str] = &["provider_accounts", "engine_logins"];
/// Also never created by a fresh #87 server; upgrades keep their rows so old
/// turns still show their fallback model and account-change questions.
const NOT_CREATED: &[&str] = &[
    "provider_accounts",
    "engine_logins",
    "model_fallbacks",
    "provider_account_changes",
];

fn diff(actual: &Value, expected: &Value) -> String {
    let names = |value: &Value| -> Vec<String> {
        value["objects"]
            .as_array()
            .map(|rows| rows.iter().map(Value::to_string).collect())
            .unwrap_or_default()
    };
    let (left, right) = (names(actual), names(expected));
    let extra: Vec<&String> = left.iter().filter(|row| !right.contains(row)).collect();
    let missing: Vec<&String> = right.iter().filter(|row| !left.contains(row)).collect();
    format!(
        "unexpected {extra:?}; missing {missing:?}; application ids {} vs {}",
        actual["applicationId"], expected["applicationId"]
    )
}

async fn fresh_startup_extends_the_schema_and_reopens_its_data_unchanged(mode: Mode) -> Result<()> {
    let (frozen, full) = expected(mode)?;
    let current = without(&full, NOT_CREATED);
    let upgraded_current = without(&full, DROPPED_BY_UPGRADE);
    let fresh = TestRoot::new()?;
    start_and_close(&fresh, mode).await?;
    {
        let db = Inspector::open(fresh.database())?;
        let actual = inspector_snapshot(&db)?;
        ensure!(
            actual == current,
            "fresh schema: {}",
            diff(&actual, &current)
        );
        if mode == Mode::Canonical {
            ensure!(
                db.scalar::<i64>("SELECT max(version) FROM schema_migrations", [])? == 7,
                "migration version"
            );
            ensure!(
                db.scalar::<String>("SELECT serving_state FROM authority_state", [])? == "active",
                "serving state"
            );
        }
    }

    // Set MINARBOT_SCHEMA_BASELINE_BINARY in the Node suite to compare with a
    // real 85373b3 start; this port seeds the frozen fixture directly.
    let (old, _canonical) = match mode {
        Mode::Canonical => {
            let canonical = Canonical::new()?;
            (canonical.root.clone(), Some(canonical))
        }
        Mode::Local => {
            let root = TestRoot::new()?;
            root.dir("data")?;
            Fixture::open(root.database())?.execute_batch(LOCAL)?;
            (root, None)
        }
    };
    let existing = Fixture::open(old.database())?;
    let seeded = fixture_snapshot(&existing)?;
    ensure!(
        seeded == frozen,
        "frozen fixture: {}",
        diff(&seeded, &frozen)
    );
    existing.execute_batch(
        "INSERT INTO agents(id,name,model,instructions) VALUES('fixture-agent','Saved agent','fixture','Keep data');
         INSERT INTO agent_creations VALUES('fixture-agent','{}');
         INSERT INTO turns(id,agent,text,reply,status,created) VALUES('fixture-turn','fixture-agent','Question','Saved answer','done','2026-11-22');
         INSERT INTO agent_computers VALUES('fixture-agent',1,2);
         INSERT INTO computer_always_on VALUES('fixture-agent');",
    )?;
    let saved = existing.row(RETAINED, [])?;
    ensure!(saved["reply"] == "Saved answer", "seeded row: {saved}");
    let before: i64 = existing.scalar("PRAGMA schema_version", [])?;
    start_and_close(&old, mode).await?;
    let upgraded = fixture_snapshot(&existing)?;
    ensure!(
        upgraded == upgraded_current,
        "upgraded schema: {}",
        diff(&upgraded, &upgraded_current)
    );
    let after: i64 = existing.scalar("PRAGMA schema_version", [])?;
    ensure!(after != before, "upgrade did not change the schema version");
    start_and_close(&old, mode).await?;
    let reopened = fixture_snapshot(&existing)?;
    ensure!(
        reopened == upgraded_current,
        "reopened schema: {}",
        diff(&reopened, &upgraded_current)
    );
    ensure!(
        existing.scalar::<i64>("PRAGMA schema_version", [])? == after,
        "second start changed the schema"
    );
    let mut retained = existing.row(RETAINED, [])?;
    if let Some(fields) = retained.as_object_mut() {
        fields.remove("provider_instance");
        fields.remove("model_options");
    }
    let mut expected_row = saved.clone();
    if let Some(fields) = expected_row.as_object_mut() {
        for column in ["runtime", "provider_account", "backup_model"] {
            fields.remove(column);
        }
        fields.insert("library_docs_access".into(), json!(1));
        if mode == Mode::Local {
            fields.insert("learn_from_conversations".into(), json!(1));
            fields.insert("memory_model".into(), json!(""));
        }
    }
    ensure!(
        retained == expected_row,
        "retained row: {retained} expected {expected_row}"
    );
    if mode == Mode::Canonical {
        ensure!(
            existing.scalar::<i64>("SELECT max(version) FROM schema_migrations", [])? == 7,
            "upgraded migration version"
        );
    }
    Ok(())
}

#[tokio::test]
async fn fresh_canonical_startup_extends_the_85373b3_schema_and_reopens_its_data_unchanged(
) -> Result<()> {
    fresh_startup_extends_the_schema_and_reopens_its_data_unchanged(Mode::Canonical).await
}

#[tokio::test]
async fn fresh_local_startup_extends_the_85373b3_schema_and_reopens_its_data_unchanged(
) -> Result<()> {
    fresh_startup_extends_the_schema_and_reopens_its_data_unchanged(Mode::Local).await
}

#[tokio::test]
async fn local_0_16_4_memory_import_does_not_backfill_learning_jobs() -> Result<()> {
    // Set MINARBOT_V0164_BINARY in the Node suite for a real 0.16.4 start;
    // this port seeds the 0.16.4 schema from the frozen fixture.
    let root = TestRoot::new()?;
    root.dir("data")?;
    let original = Fixture::open(root.database())?;
    original.execute_batch(LOCAL)?;
    original.execute_batch(
        "INSERT INTO agents(id,name,model,instructions) VALUES('learning-agent','Learning agent','agent-model','');
         INSERT INTO turns(id,agent,text,reply,status,created) VALUES('historical-turn','learning-agent','Old question','Old answer','done','2026-11-22');",
    )?;
    add_memory_schema(&original, Mode::Local, false)?;
    original.execute_batch(
        "INSERT INTO memory_imports VALUES(1); INSERT INTO memory_import SELECT * FROM memory_history_rows;",
    )?;
    ensure!(
        original.scalar::<i64>("SELECT count(*) FROM memory_units", [])? == 2,
        "imported units"
    );
    drop(original);
    start_and_close(&root, Mode::Local).await?;
    let reopened = Fixture::open(root.database())?;
    ensure!(
        reopened.scalar::<i64>("SELECT count(*) FROM memory_learning_turns", [])? == 0,
        "history backfilled learning turns"
    );
    ensure!(
        reopened.scalar::<i64>("SELECT count(*) FROM memory_extraction", [])? == 0,
        "history backfilled extraction"
    );
    reopened.execute_batch(
        "UPDATE turns SET reply='Edited answer' WHERE id='historical-turn';
         INSERT INTO turns(id,agent,text,status,created) VALUES('new-turn','learning-agent','New question','queued','2026-11-22');
         UPDATE turns SET status='done',reply='New answer' WHERE id='new-turn';",
    )?;
    ensure!(
        reopened.scalar::<i64>("SELECT count(*) FROM memory_learning_turns", [])? == 1,
        "new turn not queued for learning"
    );
    let queued: Vec<Value> = reopened
        .rows(
            "SELECT source_id,origin FROM memory_extraction JOIN memory_units ON raw=id ORDER BY origin",
            [],
        )?
        .into_iter()
        .map(|row| json!([row["source_id"], row["origin"]]))
        .collect();
    ensure!(
        queued == [json!(["new-turn", "prompt"]), json!(["new-turn", "reply"])],
        "queued extraction: {queued:?}"
    );
    ensure!(
        reopened.scalar::<i64>(
            "SELECT count(*) FROM memory_units JOIN memory_fts ON memory_fts.rowid=id \
             WHERE source_id='historical-turn' AND memory_fts MATCH 'question'",
            [],
        )? == 1,
        "historical text not searchable"
    );
    reopened.execute_batch(
        "INSERT INTO agents(id,name,model,instructions,learn_from_conversations) VALUES('paused-agent','Paused agent','agent-model','',0);
         INSERT INTO turns(id,agent,text,reply,status,created) VALUES('paused-turn','paused-agent','No learn','No learn','queued','2026-11-22');
         UPDATE turns SET status='done' WHERE id='paused-turn';",
    )?;
    ensure!(
        reopened.scalar::<i64>(
            "SELECT count(*) FROM memory_learning_turns WHERE agent='paused-agent'",
            [],
        )? == 0,
        "paused agent queued for learning"
    );
    ensure!(
        reopened.scalar::<i64>(
            "SELECT count(*) FROM memory_units WHERE source_id='paused-turn'",
            []
        )? == 2,
        "paused agent turn not stored as memory units"
    );
    Ok(())
}
