//! Shared SQL through `query_data`: schema ownership, host restrictions and
//! bounded results (port of `app/shared-data.test.ts`).

use crate::sa_support::{after_tool, api_agent, id, lock, Shared};
use minarbot_testkit::{
    model::{Reply, ScriptedModel},
    prelude::*,
    Server,
};
use std::time::{Duration, Instant};

struct Sql {
    app: Server,
    args: Shared<Value>,
}

impl Sql {
    /// Run `sql` as `owner` through one agent turn; `Ok(Err(message))` is a
    /// tool error the model saw.
    async fn execute(
        &self,
        sql: &str,
        owner: &str,
        parameters: Value,
    ) -> Result<std::result::Result<Value, String>> {
        *lock(&self.args)? = json!({"sql": sql, "parameters": parameters});
        let path = format!("/api/agents/{owner}/turns");
        let turn = new_id();
        self.app
            .post_expect(&path, json!({"id": turn, "text": "Run shared query"}), 202)
            .await?;
        let row = self
            .app
            .wait_row(&path, &turn, |row| {
                row["status"] == "done" || row["status"] == "failed"
            })
            .await?;
        ensure!(
            row["status"] == "done",
            "shared SQL turn failed for {sql}: {row}"
        );
        let reply = row["reply"]
            .as_str()
            .ok_or_else(|| format!("no reply: {row}"))?;
        let value: Value = serde_json::from_str(reply)
            .map_err(|error| format!("reply is not JSON ({error}): {reply}"))?;
        Ok(match value["error"].as_str() {
            Some(error) => Err(error.to_owned()),
            None => Ok(value),
        })
    }

    async fn ok(&self, sql: &str, owner: &str) -> Result<Value> {
        self.ok_with(sql, owner, json!([])).await
    }

    async fn ok_with(&self, sql: &str, owner: &str, parameters: Value) -> Result<Value> {
        match self.execute(sql, owner, parameters).await? {
            Ok(value) => Ok(value),
            Err(error) => Err(format!("{sql} failed: {error}").into()),
        }
    }

    /// Require a tool error containing one of `needles` (case-insensitive).
    async fn rejects(
        &self,
        sql: &str,
        owner: &str,
        parameters: Value,
        needles: &[&str],
    ) -> Result<()> {
        match self.execute(sql, owner, parameters).await? {
            Ok(value) => Err(format!("{sql} succeeded: {value}").into()),
            Err(error) => {
                let lower = error.to_lowercase();
                ensure!(
                    needles
                        .iter()
                        .any(|needle| lower.contains(&needle.to_lowercase())),
                    "{sql}: error {error:?} matches none of {needles:?}"
                );
                Ok(())
            }
        }
    }

    async fn list(&self, after: &str) -> Result<Value> {
        let mut encoded = String::new();
        for byte in after.bytes() {
            if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
                encoded.push(char::from(byte));
            } else {
                encoded.push_str(&format!("%{byte:02X}"));
            }
        }
        self.app.get(&format!("/api/data?after={encoded}")).await
    }
}

const DENIED: &[&str] = &["authorized", "prohibited"];

#[tokio::test]
async fn rust_shared_sql_keeps_schema_ownership_host_restrictions_and_bounded_results() -> Result<()>
{
    let model = ScriptedModel::start().await?;
    let args: Shared<Value> = Shared::default();
    let current = args.clone();
    model.respond(move |request| {
        if after_tool(request) {
            let content = request
                .tool_results()
                .last()
                .map(|message| message["content"].clone())
                .unwrap_or(Value::Null);
            return Reply::text(content.as_str().unwrap_or_default());
        }
        let arguments = current
            .lock()
            .map(|args| args.clone())
            .unwrap_or(Value::Null);
        Reply::tool_call("sql", "query_data", arguments)
    });
    let root = TestRoot::new()?;
    let app = root.start(ServerConfig::new().model(&model)).await?;
    let alpha = id(&api_agent(&app, "Alpha").await?)?;
    let beta = id(&api_agent(&app, "Beta").await?)?;
    let sql = Sql { app, args };

    sql.ok(
        "CREATE TABLE notes(id INTEGER PRIMARY KEY, text TEXT)",
        &alpha,
    )
    .await?;
    sql.ok_with(
        "INSERT INTO notes(text) VALUES (?)",
        &beta,
        json!(["shared"]),
    )
    .await?;
    sql.rejects(
        "ALTER TABLE notes RENAME TO minarbot_hidden",
        &alpha,
        json!([]),
        &["Table name is protected or exceeds 128 characters."],
    )
    .await?;
    sql.rejects(
        &format!("ALTER TABLE notes RENAME TO \"{}\"", "n".repeat(129)),
        &alpha,
        json!([]),
        &["128"],
    )
    .await?;
    ensure!(
        sql.ok("SELECT * FROM notes", &beta).await?["rows"] == json!([[1, "shared"]]),
        "shared rows"
    );
    for statement in [
        "DROP TABLE notes",
        "ALTER TABLE notes ADD COLUMN private TEXT",
        "CREATE INDEX bad_index ON notes(text)",
    ] {
        sql.rejects(statement, &beta, json!([]), DENIED).await?;
    }
    sql.ok("ALTER TABLE notes ADD COLUMN n INTEGER", &alpha)
        .await?;
    sql.ok("ALTER TABLE notes RENAME TO renamed", &alpha)
        .await?;
    sql.rejects("DROP TABLE renamed", &beta, json!([]), DENIED)
        .await?;
    sql.ok("CREATE INDEX note_index ON renamed(text)", &alpha)
        .await?;
    sql.rejects("DROP INDEX note_index", &beta, json!([]), DENIED)
        .await?;
    sql.ok("DROP INDEX note_index", &alpha).await?;

    for (name, owner, other) in [
        ("minarbotXnotes", &beta, &alpha),
        ("sqliteXnotes", &beta, &alpha),
        ("\u{c9}", &alpha, &beta),
        ("\u{e9}", &beta, &alpha),
        ("k", &alpha, &beta),
        ("\u{212a}", &beta, &alpha),
    ] {
        sql.ok(&format!("CREATE TABLE \"{name}\"(x)"), owner)
            .await?;
        sql.rejects(
            &format!("ALTER TABLE \"{name}\" ADD COLUMN y"),
            other,
            json!([]),
            &["authorized"],
        )
        .await?;
        sql.ok(&format!("ALTER TABLE \"{name}\" ADD COLUMN y"), owner)
            .await?;
    }
    let named = sql.list("").await?["tables"].clone();
    let tables = named.as_array().cloned().unwrap_or_default();
    ensure!(tables.len() == 7, "tables: {named}");
    for table in tables.iter().filter(|table| table["name"] != "renamed") {
        let (name, owner) = (
            table["name"].as_str().unwrap_or_default(),
            table["owner"].as_str().unwrap_or_default(),
        );
        sql.ok(&format!("DROP TABLE \"{name}\""), owner).await?;
    }

    for statement in [
        "ATTACH DATABASE ':memory:' AS other",
        "VACUUM INTO '/dev/null'",
        "PRAGMA writable_schema=ON",
        "SELECT * FROM minarbot_owners",
        "DELETE FROM minarbot_owners",
        "DROP TABLE minarbot_owners",
        "ALTER TABLE minarbot_owners RENAME TO blocked",
        "CREATE TABLE minarbot_hidden(x)",
        "CREATE TEMP TABLE temp_data(x)",
        "CREATE VIEW view_data AS SELECT * FROM renamed",
        "CREATE TRIGGER trigger_data AFTER INSERT ON renamed BEGIN DELETE FROM renamed; END",
        "SELECT load_extension('/tmp/not-an-extension')",
        "BEGIN",
        "CREATE VIRTUAL TABLE search USING fts5(text)",
    ] {
        sql.rejects(
            statement,
            &alpha,
            json!([]),
            &["authorized", "prohibited", "attached", "transaction"],
        )
        .await?;
    }
    let revision = sql.list("").await?["revision"].clone();
    let readonly = sql
        .app
        .post_expect(
            "/api/data/query",
            json!({"id": new_id(), "revision": revision,
                "sql": "INSERT INTO renamed(text) VALUES ('no')"}),
            400,
        )
        .await?;
    let error = readonly["error"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    ensure!(
        error.contains("authorized") || error.contains("prohibited"),
        "read-only query: {readonly}"
    );
    let large = sql
        .ok(
            "SELECT 9223372036854775807 AS exact, x'00ff' AS bytes",
            &alpha,
        )
        .await?;
    ensure!(
        large["rows"] == json!([[{"integer": "9223372036854775807"}, {"base64": "AP8="}]]),
        "exact values: {large}"
    );
    sql.rejects(
        "INSERT INTO renamed(text) VALUES (hex(zeroblob(100000))) RETURNING text",
        &alpha,
        json!([]),
        &["128 KiB"],
    )
    .await?;
    ensure!(
        sql.ok("SELECT text FROM renamed ORDER BY id", &alpha)
            .await?["rows"]
            == json!([["shared"]]),
        "rolled back insert"
    );
    ensure!(
        sql.list("").await?["revision"] == large["revision"],
        "read changed the revision"
    );
    // The bound is on the query; subtract the cost of an ordinary harness turn.
    let started = Instant::now();
    sql.ok("SELECT 1", &alpha).await?;
    let baseline = started.elapsed();
    let started = Instant::now();
    sql.rejects(
        "WITH RECURSIVE count(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM count) SELECT sum(n) FROM count",
        &alpha,
        json!([]),
        &["interrupted", "timed out", "resource"],
    )
    .await?;
    let runaway = started.elapsed();
    ensure!(
        runaway.saturating_sub(baseline) < Duration::from_secs(5),
        "runaway query took {runaway:?} (ordinary turn {baseline:?})"
    );
    // Number.MAX_SAFE_INTEGER + 1 is not a safe integer parameter.
    sql.rejects(
        "SELECT ?",
        &alpha,
        json!([9_007_199_254_740_992_u64]),
        &["parameters"],
    )
    .await?;
    sql.rejects(
        "SELECT 1; DELETE FROM renamed",
        &alpha,
        json!([]),
        &["one statement"],
    )
    .await?;
    let listing = sql.list("").await?;
    let pairs: Vec<(Value, Value)> = listing["tables"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| (row["name"].clone(), row["owner"].clone()))
                .collect()
        })
        .unwrap_or_default();
    ensure!(
        pairs == [(json!("renamed"), json!(alpha))],
        "listing: {listing}"
    );
    sql.app
        .post_expect(
            "/api/data/drop",
            json!({"id": new_id(), "revision": listing["revision"], "table": "renamed"}),
            200,
        )
        .await?;
    ensure!(sql.list("").await?["tables"] == json!([]), "drop");

    let expected: Vec<String> = (0..21).map(|n| format!("page_{n:02}")).collect();
    let column = "long_column_".repeat(500);
    for name in &expected {
        sql.ok(&format!("CREATE TABLE {name}(\"{column}\" TEXT)"), &alpha)
            .await?;
    }
    let mut names = Vec::new();
    let mut after = String::new();
    loop {
        let page = sql.list(&after).await?;
        ensure!(
            page.to_string().len() < 128 * 1024,
            "page is {} bytes",
            page.to_string().len()
        );
        names.extend(
            page["tables"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|row| row["name"].as_str().map(str::to_owned)),
        );
        after = page["next_after"].as_str().unwrap_or_default().to_owned();
        if after.is_empty() {
            break;
        }
    }
    ensure!(names == expected, "paged names: {names:?}");
    Ok(())
}
