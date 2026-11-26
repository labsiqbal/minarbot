//! History paging and search (port of `app/history.test.ts`).

use minarbot_testkit::{
    http::Method,
    model::{Reply, ScriptedModel},
    prelude::*,
    providers::AgentSpec,
};
use std::collections::HashSet;

async fn page(app: &Server, request: &str) -> Result<Vec<Value>> {
    let body = app.get_expect(request, 200).await?;
    let Value::Array(rows) = body else {
        return Err(format!("GET {request}: expected an array, got {body}").into());
    };
    Ok(rows)
}

fn ids(rows: &[Value]) -> Vec<&Value> {
    rows.iter().map(|row| &row["id"]).collect()
}

#[tokio::test]
async fn history_pages_preserve_order_and_conversation_boundaries_across_restart() -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(|_| Reply::text("Done."));
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new().model(&model)).await?;
    let agent = AgentSpec::new("Reader").runtime("api").create(&app).await?;
    let agent_id = agent["id"].as_str().ok_or("missing agent id")?;
    let channel = app
        .post(
            "/api/channels",
            json!({"id": new_id(), "name": "History", "instructions": "Help.",
                "lead": agent_id, "members": [agent_id]}),
        )
        .await?;
    let channel_id = channel["id"].as_str().ok_or("missing channel id")?;
    let side = app
        .post_expect(
            &format!("/api/agents/{agent_id}/threads"),
            json!({"id": new_id(), "name": "Side"}),
            200,
        )
        .await?;
    let side_id = side["id"].as_str().ok_or("missing side-thread id")?;
    let paths = [
        format!("/api/agents/{agent_id}/turns"),
        format!("/api/channels/{channel_id}/turns"),
        format!("/api/threads/{side_id}/turns"),
    ];
    for path in &paths {
        for n in 0..205 {
            let id = new_id();
            app.post_expect(
                path,
                json!({"id": id, "text": format!("{path} message {n}")}),
                202,
            )
            .await?;
            app.wait_status(&format!("{path}?focus={id}"), &id, "done")
                .await?;
        }
    }
    app.restart().await?;

    for path in &paths {
        let recent = page(&app, path).await?;
        ensure!(
            recent.len() == 100,
            "GET {path}: expected 100 rows, got {}",
            recent.len()
        );
        let first_text = format!("{path} message 105");
        ensure!(
            recent[0]["text"] == first_text,
            "GET {path}: expected first text {first_text:?}, got {}",
            recent[0]["text"]
        );
        let before_recent = format!("{path}?before={}", recent[0]["seq"]);
        let older = page(&app, &before_recent).await?;
        ensure!(
            older.len() == 100,
            "GET {before_recent}: expected 100 rows, got {}",
            older.len()
        );
        let before_older = format!("{path}?before={}", older[0]["seq"]);
        let oldest = page(&app, &before_older).await?;
        ensure!(
            oldest.len() == 5,
            "GET {before_older}: expected 5 rows, got {}",
            oldest.len()
        );
        let all: Vec<_> = oldest.iter().chain(&older).chain(&recent).collect();
        let texts: Vec<_> = all.iter().map(|row| row["text"].clone()).collect();
        let expected: Vec<_> = (0..205)
            .map(|n| json!(format!("{path} message {n}")))
            .collect();
        ensure!(
            texts == expected,
            "GET {path}, {before_recent}, and {before_older}: expected message 0 through 204 in order, got {texts:?}"
        );
        let before_oldest = format!("{path}?before={}", oldest[0]["seq"]);
        let empty = page(&app, &before_oldest).await?;
        ensure!(
            empty.is_empty(),
            "GET {before_oldest}: expected no rows, got {}",
            empty.len()
        );
        let unique: HashSet<_> = all.iter().filter_map(|row| row["id"].as_str()).collect();
        ensure!(
            unique.len() == 205,
            "GET {path}: expected 205 unique string IDs, got {}",
            unique.len()
        );

        let search_path = format!("{path}?q=message%201");
        let search = page(&app, &search_path).await?;
        ensure!(
            search.len() == 100,
            "GET {search_path}: expected 100 rows, got {}",
            search.len()
        );
        let search_before = format!("{search_path}&before={}", search[0]["seq"]);
        let search_older = page(&app, &search_before).await?;
        let search_ids = ids(&search);
        let found: Vec<_> = search_older
            .iter()
            .chain(&search)
            .map(|row| &row["id"])
            .collect();
        let matching: Vec<_> = all
            .iter()
            .filter(|row| {
                row["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("message 1"))
            })
            .map(|row| &row["id"])
            .collect();
        ensure!(
            found == matching,
            "GET {search_path} and {search_before}: expected IDs {matching:?}, got {found:?}"
        );
        let reply_path = format!("{path}?q=Done.");
        let replies = page(&app, &reply_path).await?;
        ensure!(
            replies.len() == 100,
            "GET {reply_path}: expected 100 rows, got {}",
            replies.len()
        );
        let leading_path = format!("{path}?q=%20message%201");
        let leading = page(&app, &leading_path).await?;
        ensure!(
            ids(&leading) == search_ids,
            "GET {leading_path}: expected IDs {search_ids:?}, got {:?}",
            ids(&leading)
        );
        let trailing_path = format!("{path}?q=%20message%201%20");
        let trailing = page(&app, &trailing_path).await?;
        ensure!(
            trailing.is_empty(),
            "GET {trailing_path}: expected no rows, got {}",
            trailing.len()
        );
        let upper_path = format!("{path}?q=MESSAGE");
        let upper = page(&app, &upper_path).await?;
        ensure!(
            upper.len() == 100,
            "GET {upper_path}: expected 100 rows, got {}",
            upper.len()
        );

        let oldest_id = &oldest[0]["id"];
        let focus = oldest_id.as_str().ok_or("missing oldest turn id")?;
        let focus_path = format!("{path}?focus={focus}");
        let selected = page(&app, &focus_path).await?;
        ensure!(
            ids(&selected) == [oldest_id],
            "GET {focus_path}: expected ID {focus}, got {:?}",
            ids(&selected)
        );
        for foreign in paths.iter().filter(|other| *other != path) {
            let request = format!("{foreign}?focus={focus}");
            let reply = app.call(Method::GET, &request, None).await?;
            ensure!(
                reply.status == 404,
                "GET {request}: expected HTTP 404, got {}: {}",
                reply.status,
                reply.body
            );
        }
        for query in ["", "%20", &"a".repeat(201)] {
            let request = format!("{path}?q={query}");
            let reply = app.call(Method::GET, &request, None).await?;
            ensure!(
                reply.status == 400,
                "GET {request}: expected HTTP 400, got {}: {}",
                reply.status,
                reply.body
            );
        }
        let conflict = format!("{path}?q=message&focus={focus}");
        let reply = app.call(Method::GET, &conflict, None).await?;
        ensure!(
            reply.status == 400,
            "GET {conflict}: expected HTTP 400, got {}: {}",
            reply.status,
            reply.body
        );
        let invalid_focus = format!("{path}?focus=invalid");
        let reply = app.call(Method::GET, &invalid_focus, None).await?;
        ensure!(
            reply.status == 404,
            "GET {invalid_focus}: expected HTTP 404, got {}: {}",
            reply.status,
            reply.body
        );

        let literal_id = json!(new_id());
        app.post_expect(
            path,
            json!({"id": literal_id, "text": "100%_ ' OR 1=1 -- 日本語"}),
            202,
        )
        .await?;
        let literal_path =
            format!("{path}?q=100%25_%20%27%20OR%201%3D1%20--%20%E6%97%A5%E6%9C%AC%E8%AA%9E");
        let literal = page(&app, &literal_path).await?;
        ensure!(
            ids(&literal) == [&literal_id],
            "GET {literal_path}: expected ID {literal_id}, got {:?}",
            ids(&literal)
        );
        app.post_expect(path, json!({"id": new_id(), "text": "New arrival"}), 202)
            .await?;
        let stable = page(&app, &before_recent).await?;
        ensure!(
            ids(&stable) == ids(&older),
            "GET {before_recent} after new arrival: expected IDs {:?}, got {:?}",
            ids(&older),
            ids(&stable)
        );
        for cursor in ["0", "-1", "1.5", "NaN", "9007199254740992", ""] {
            let request = format!("{path}?before={cursor}");
            let reply = app.call(Method::GET, &request, None).await?;
            ensure!(
                reply.status == 400,
                "GET {request}: expected HTTP 400, got {}: {}",
                reply.status,
                reply.body
            );
        }
    }

    let request = format!("{}?before=2&focus={}", paths[1], new_id());
    let reply = app.call(Method::GET, &request, None).await?;
    ensure!(
        reply.status == 400,
        "GET {request}: expected HTTP 400, got {}: {}",
        reply.status,
        reply.body
    );
    Ok(())
}
