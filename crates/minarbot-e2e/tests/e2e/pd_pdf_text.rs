//! PDF text extraction in previews and messages (port of `app/pdf-text.test.ts`).

use minarbot_testkit::{
    http::Method,
    model::ModelRequest,
    parked::{Answer, ParkedModel, Route},
    pdf::{pdf_fixture, PdfGate},
    prelude::*,
    server::Gateway,
};
use std::sync::{Arc, Mutex};

const ROUTING: &str = "Choose exactly one channel member";

#[derive(Default)]
struct Plan {
    target: String,
    tool: bool,
    generated: u8,
}

fn route(plan: &Arc<Mutex<Plan>>, request: &ModelRequest) -> Route {
    if request.last_text() == "BLOCK_EDIT" {
        return Route::Park("block".into());
    }
    if request.system().starts_with(ROUTING) {
        let data: Value = serde_json::from_str(&request.last_text()).unwrap_or(Value::Null);
        return Route::Now(Answer::text(
            json!({"action": "assign", "recipient": data["members"][0]["id"]}).to_string(),
        ));
    }
    let Ok(mut plan) = plan.lock() else {
        return Route::Now(Answer::fail("poisoned"));
    };
    if plan.generated == 1 {
        plan.generated = 2;
        let text =
            String::from_utf8_lossy(&pdf_fixture("GENERATED_PDF_TEXT", 1, false)).into_owned();
        return Route::Now(Answer::tool(
            "create_text_file",
            json!({"name": "generated.pdf", "text": text}),
        ));
    }
    if plan.generated == 2 || plan.tool {
        if plan.generated == 2 {
            let created: Value = serde_json::from_str(&request.last_text()).unwrap_or(Value::Null);
            plan.target = created["id"].as_str().unwrap_or("").to_owned();
        }
        plan.generated = 0;
        plan.tool = false;
        return Route::Now(Answer::tool("read_attachment", json!({"id": plan.target})));
    }
    Route::Now(Answer::text("PDF accepted"))
}

struct Pdf {
    app: Server,
    model: ParkedModel,
    plan: Arc<Mutex<Plan>>,
    a: String,
}

impl Pdf {
    fn change(&self, change: impl FnOnce(&mut Plan)) {
        if let Ok(mut plan) = self.plan.lock() {
            change(&mut plan);
        }
    }

    fn payloads(&self) -> Vec<ModelRequest> {
        self.model.requests()
    }

    fn last_payload(&self) -> String {
        self.payloads()
            .last()
            .map(|request| request.body.to_string())
            .unwrap_or_default()
    }

    async fn upload(&self, scope: &str, bytes: &[u8]) -> Result<String> {
        let id = new_id();
        let response = self
            .app
            .request(Method::POST, &format!("{scope}/files/{id}"))?
            .header("Content-Type", "application/octet-stream")
            .header("X-File-Name", "renamed.bin")
            .body(bytes.to_vec())
            .send()
            .await?;
        ensure!(
            response.status().as_u16() == 200,
            "upload: {}",
            response.status()
        );
        Ok(id)
    }

    fn command(&self, scope: &str, text: &str, files: &[&str]) -> Value {
        let mut command = json!({"id": new_id(), "text": text});
        if !files.is_empty() {
            command["files"] = json!(files);
        }
        if scope.contains("/channels/") {
            command["recipient"] = json!(self.a);
        }
        command
    }

    /// Wait until the turn settles; it must be `done`.
    async fn done(&self, scope: &str, id: &Value) -> Result<()> {
        let id = id.as_str().ok_or("turn id")?;
        let turn = self
            .app
            .wait_row(&format!("{scope}/turns"), id, |row| {
                ["done", "failed"].contains(&row["status"].as_str().unwrap_or(""))
            })
            .await?;
        ensure!(turn["status"] == "done", "turn failed: {}", turn["error"]);
        Ok(())
    }

    async fn send(&self, scope: &str, command: &Value) -> Result<()> {
        self.app
            .post(&format!("{scope}/turns"), command.clone())
            .await?;
        self.done(scope, &command["id"]).await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pdf_http_preview_and_messages_preserve_source_scope_retry_routing_tools_and_restart(
) -> Result<()> {
    let plan: Arc<Mutex<Plan>> = Arc::default();
    let routed = plan.clone();
    let model = ParkedModel::start(move |request| route(&routed, request)).await?;
    let gate = PdfGate::start().await?;
    let token = "p".repeat(64);
    let config = ServerConfig::new()
        .gateway(Gateway::new(model.base_url()))
        .pdf_bridge(gate.bridge())
        .option("scriptToken", token.clone());
    let root = TestRoot::new()?;
    let app = root.start(config.clone()).await?;
    let a = app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "PDF agent", "model": "fixture",
                "instructions": "Read supplied files."}),
            201,
        )
        .await?;
    let b = app
        .post_expect(
            "/api/agents",
            json!({"runtime": "api", "id": new_id(), "name": "Other", "model": "fixture",
                "instructions": "Read supplied files."}),
            201,
        )
        .await?;
    let a_id = a["id"].as_str().ok_or("a id")?.to_owned();
    let thread = app
        .post(
            &format!("/api/agents/{a_id}/threads"),
            json!({"id": new_id(), "name": "Side"}),
        )
        .await?;
    let channel = app
        .post_expect(
            "/api/channels",
            json!({"id": new_id(), "name": "PDF channel", "instructions": "Read supplied files.",
                "members": [a_id, b["id"]], "lead": a_id}),
            201,
        )
        .await?;
    let scopes = [
        format!("/api/agents/{a_id}"),
        format!("/api/threads/{}", thread["id"].as_str().ok_or("thread id")?),
        format!(
            "/api/channels/{}",
            channel["id"].as_str().ok_or("channel id")?
        ),
    ];
    let mut pdf = Pdf {
        app,
        model,
        plan,
        a: a_id.clone(),
    };

    // Each scope previews, sends and rereads its own PDF.
    let mut files = Vec::new();
    for (index, scope) in scopes.iter().enumerate() {
        let marker = format!("PDF_SCOPE_{index}");
        let bytes = pdf_fixture(&marker, 1, true);
        let file = pdf.upload(scope, &bytes).await?;
        let preview_path = format!("{scope}/files/{file}?preview=text");
        let (first, second) = tokio::join!(pdf.app.get(&preview_path), pdf.app.get(&preview_path));
        let (first, second) = (first?, second?);
        ensure!(first["source"] == "pdf-text", "preview: {first}");
        ensure!(
            first["text"] == second["text"],
            "concurrent previews differ"
        );
        ensure!(
            first["text"]
                .as_str()
                .is_some_and(|text| text.contains(&marker)),
            "preview text: {}",
            first["text"]
        );
        let download = pdf
            .app
            .client()?
            .get(pdf.app.url(&format!("{scope}/files/{file}")))
            .send()
            .await?
            .bytes()
            .await?;
        ensure!(download.to_vec() == bytes, "download differs");
        let command = pdf.command(scope, "Read PDF", &[&file]);
        let before = pdf.payloads().len();
        pdf.send(scope, &command).await?;
        for payload in pdf.payloads().split_off(before) {
            let text = payload.body.to_string();
            ensure!(
                text.contains(&marker),
                "{marker} missing from a model request"
            );
            ensure!(
                text.contains("Extracted text only"),
                "extraction notice missing"
            );
        }
        let count = pdf.payloads().len();
        pdf.app.post(&format!("{scope}/turns"), command).await?;
        ensure!(pdf.payloads().len() == count, "replay reached the model");
        pdf.change(|plan| {
            plan.target.clone_from(&file);
            plan.tool = true;
        });
        let read = pdf.command(scope, "Reread PDF", &[]);
        pdf.send(scope, &read).await?;
        ensure!(pdf.last_payload().contains(&marker), "reread lost {marker}");
        files.push(file);
    }
    let direct = scopes[0].clone();
    pdf.app
        .get_expect(&format!("{direct}/files/{}?preview=text", files[1]), 404)
        .await?;
    pdf.app
        .post_expect(
            &format!("{direct}/turns"),
            json!({"id": new_id(), "text": "Foreign", "files": [files[1]]}),
            404,
        )
        .await?;
    let no_preview = pdf
        .upload(&direct, &pdf_fixture("AUTO_EXTRACT", 1, true))
        .await?;
    let auto = pdf.command(&direct, "No preview needed", &[&no_preview]);
    pdf.send(&direct, &auto).await?;
    ensure!(pdf.last_payload().contains("AUTO_EXTRACT"), "auto extract");

    // A sandbox failure rejects the send without storing it; the retry works.
    let retry_file = pdf
        .upload(&direct, &pdf_fixture("RETRY_SANDBOX", 1, true))
        .await?;
    let retry = pdf.command(&direct, "Retry after sandbox repair", &[&retry_file]);
    gate.fail_once();
    pdf.app
        .post_expect(&format!("{direct}/turns"), retry.clone(), 503)
        .await?;
    ensure!(
        pdf.app
            .row(
                &format!("{direct}/turns"),
                retry["id"].as_str().unwrap_or("")
            )
            .await?
            .is_none(),
        "failed send was stored"
    );
    pdf.send(&direct, &retry).await?;
    ensure!(
        pdf.last_payload().contains("RETRY_SANDBOX"),
        "retry extract"
    );

    // A queued message edited to attach a PDF sends the PDF text.
    let block = pdf.command(&direct, "BLOCK_EDIT", &[]);
    pdf.app.post(&format!("{direct}/turns"), block).await?;
    pdf.model.wait_parked(&["block"]).await?;
    let queued = pdf
        .app
        .post(
            &format!("{direct}/turns"),
            json!({"id": new_id(), "text": "Original queued message"}),
        )
        .await?;
    let editor = new_id();
    let edit_path = format!(
        "{direct}/turns/{}/edit",
        queued["id"].as_str().ok_or("queued id")?
    );
    let hold = pdf
        .app
        .post(
            &edit_path,
            json!({"id": new_id(), "editor": editor, "revision": queued["revision"],
                "action": "hold"}),
        )
        .await?;
    let edit_file = pdf
        .upload(&direct, &pdf_fixture("QUEUED_PDF_TEXT", 1, true))
        .await?;
    let save = json!({"id": new_id(), "editor": editor, "revision": hold["revision"],
        "action": "save", "text": "Read queued PDF", "files": [edit_file]});
    pdf.app.post(&edit_path, save.clone()).await?;
    pdf.app.post(&edit_path, save).await?;
    pdf.model.finish("block", Answer::text("Unblocked"))?;
    pdf.done(&direct, &queued["id"]).await?;
    ensure!(
        pdf.last_payload().contains("QUEUED_PDF_TEXT"),
        "queued edit extract"
    );
    let bad = pdf.upload(&direct, b"%PDF-malformed").await?;
    pdf.app
        .post_expect(
            &format!("{direct}/turns"),
            json!({"id": new_id(), "text": "Bad PDF", "files": [bad]}),
            415,
        )
        .await?;

    // A PDF the agent writes is read back as extracted text in every scope.
    for scope in &scopes {
        pdf.change(|plan| plan.generated = 1);
        let command = pdf.command(scope, "Make a PDF and read it", &[]);
        pdf.send(scope, &command).await?;
        let last = pdf
            .payloads()
            .last()
            .map(ModelRequest::last_text)
            .unwrap_or_default();
        let content: Value = serde_json::from_str(&last)?;
        ensure!(content["source"] == "pdf-text", "generated read: {content}");
        ensure!(
            content["text"].as_str().map(str::trim) == Some("GENERATED_PDF_TEXT"),
            "generated text: {}",
            content["text"]
        );
    }

    // Cancelling during a held parse ends the turn without a continuation.
    let reader = gate.hold();
    pdf.change(|plan| plan.generated = 1);
    let cancel = pdf.command(&direct, "Cancel generated PDF read", &[]);
    let cancel_id = cancel["id"].as_str().ok_or("id")?.to_owned();
    pdf.app.post(&format!("{direct}/turns"), cancel).await?;
    reader.started().await?;
    let count = pdf.payloads().len();
    pdf.app
        .post(
            &format!("{direct}/turns/{cancel_id}/cancel"),
            json!({"id": new_id()}),
        )
        .await?;
    reader.release();
    let cancelled = pdf
        .app
        .row(&format!("{direct}/turns"), &cancel_id)
        .await?
        .ok_or("cancelled turn")?;
    ensure!(cancelled["status"] == "cancelled", "cancelled: {cancelled}");
    let next = pdf.command(&direct, "After cancellation", &[]);
    pdf.send(&direct, &next).await?;
    ensure!(
        pdf.payloads().len() == count + 1,
        "cancelled reader continued: {} vs {count}",
        pdf.payloads().len()
    );

    // Too much extracted text: previews work, sends are refused.
    let large = pdf
        .upload(&direct, &pdf_fixture(&"X".repeat(500), 70, true))
        .await?;
    pdf.app
        .get(&format!("{direct}/files/{large}?preview=text"))
        .await?;
    pdf.app
        .post_expect(
            &format!("{direct}/turns"),
            json!({"id": new_id(), "text": "Too much extracted text", "files": [large]}),
            413,
        )
        .await?;

    // Extracted text survives a restart.
    pdf.app.restart().await?;
    let preview = pdf
        .app
        .get(&format!("{direct}/files/{}?preview=text", files[0]))
        .await?;
    ensure!(
        preview["text"]
            .as_str()
            .is_some_and(|text| text.contains("PDF_SCOPE_0")),
        "preview after restart: {preview}"
    );
    pdf.change(|plan| {
        plan.target.clone_from(&files[0]);
        plan.tool = true;
    });
    let persisted = pdf.command(&direct, "Read persisted PDF", &[]);
    pdf.send(&direct, &persisted).await?;
    ensure!(pdf.last_payload().contains("PDF_SCOPE_0"), "persisted read");

    // Archive, delete and script revocation during a parse leak nothing.
    for operation in ["send-archive", "preview-delete", "script-revoke"] {
        let side = pdf
            .app
            .post(
                &format!("/api/agents/{a_id}/threads"),
                json!({"id": new_id(), "name": operation}),
            )
            .await?;
        let side_id = side["id"].as_str().ok_or("side id")?.to_owned();
        let scope = format!("/api/threads/{side_id}");
        let file = pdf
            .upload(&scope, &pdf_fixture("DO_NOT_LEAK", 1, true))
            .await?;
        if operation == "script-revoke" {
            pdf.app
                .post(
                    &format!("/api/agents/{a_id}/scripts"),
                    json!({"id": new_id(), "revision": 0, "enabled": true}),
                )
                .await?;
        }
        let reader = gate.hold();
        let before = pdf.payloads().len();
        let app = &pdf.app;
        let pending = async {
            match operation {
                "send-archive" => {
                    let reply = app
                        .call(
                            Method::POST,
                            &format!("{scope}/turns"),
                            Some(json!({"id": new_id(), "text": "Never dispatch",
                                "files": [file]})),
                        )
                        .await?;
                    Ok::<_, minarbot_testkit::Error>((reply.status, reply.body.to_string()))
                }
                "preview-delete" => {
                    let reply = app
                        .call(
                            Method::GET,
                            &format!("{scope}/files/{file}?preview=text"),
                            None,
                        )
                        .await?;
                    Ok((reply.status, reply.body.to_string()))
                }
                _ => {
                    let response = app
                        .client()?
                        .get(app.url(&format!(
                            "/api/scripts/agents/{a_id}/threads/{side_id}/files/{file}?preview=text"
                        )))
                        .bearer_auth(&token)
                        .send()
                        .await?;
                    let status = response.status().as_u16();
                    Ok((status, response.text().await?))
                }
            }
        };
        let change = async {
            reader.started().await?;
            if operation == "script-revoke" {
                app.post(
                    &format!("/api/agents/{a_id}/scripts"),
                    json!({"id": new_id(), "revision": 1, "enabled": false}),
                )
                .await?;
            } else {
                app.post(
                    &format!("{scope}/edit"),
                    json!({"id": new_id(), "revision": 1, "name": operation, "archived": true}),
                )
                .await?;
                if operation == "preview-delete" {
                    app.post(
                        &format!("{scope}/delete"),
                        json!({"id": new_id(), "revision": 2}),
                    )
                    .await?;
                }
            }
            reader.release();
            Ok::<_, minarbot_testkit::Error>(())
        };
        let (result, changed) = tokio::join!(pending, change);
        reader.release();
        changed?;
        let (status, body) = result?;
        let expected = match operation {
            "send-archive" => 409,
            "preview-delete" => 404,
            _ => 403,
        };
        ensure!(status == expected, "{operation}: HTTP {status} {body}");
        ensure!(
            !body.contains("DO_NOT_LEAK"),
            "{operation} leaked the PDF text"
        );
        ensure!(
            pdf.payloads().len() == before,
            "{operation} reached the model"
        );
    }

    // Shutdown abandons a parse whose client already left.
    let pending_file = pdf
        .upload(&direct, &pdf_fixture("SHUTDOWN_PENDING", 1, true))
        .await?;
    let reader = gate.hold();
    let client = pdf.app.client()?;
    let url = pdf
        .app
        .url(&format!("{direct}/files/{pending_file}?preview=text"));
    let request = tokio::spawn(async move { client.get(url).send().await.map(|_| ()) });
    reader.started().await?;
    request.abort();
    let _ = request.await;
    pdf.app.shutdown().await?;
    reader.stopped().await?;
    reader.release();
    pdf.app.start_stopped(config).await?;
    Ok(())
}
