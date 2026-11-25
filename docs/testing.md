# Testing

Test each behaviour at the lowest layer that proves it. Rust unit tests cover
logic, validation, migrations, permissions, encryption and error mapping.
Rust integration tests in `crates/minarbot-e2e` cover HTTP contracts, storage,
restarts and multi-step server flows. Agent-turn tests use the kit's fake
peers and scripted model only for agent replies, tool calls, waits, resume,
streaming and approvals. UI smokes cover browser journeys.

New integration tests use `crates/minarbot-testkit`. Do not add
`app/*.test.ts` or `app/fixtures/*.mjs`.

## Build and run

The e2e tests start `target/<profile>/minarbot-server` and the kit's fake
peer binaries, so build both first. On shared hosts, run everything through
the slot script:

```sh
~/.cache/minarbot-cargo-slot.sh cargo build -p minarbot-server -p minarbot-testkit
~/.cache/minarbot-cargo-slot.sh cargo test -p minarbot-testkit -p minarbot-e2e
```

`MINARBOT_SERVER_BINARY` overrides the server path, and `MINARBOT_FAKE_BIN_DIR`
the fake peer directory. One test process runs at most two servers at a time
(`MINARBOT_TEST_SERVERS` overrides this), so waits stay meaningful on a loaded
host. `crates/minarbot-e2e/tests/e2e/main.rs` is the only test binary, with
one module per area.

## Kit overview

| Module | Use it for |
| --- | --- |
| `server` | `TestRoot` (one temp dir and database). `ServerConfig` takes every option `app/rust-test-server.ts` accepted (unknown names fail with `not ported: option X`). Builders: `fixed_clock` (on by default), `real_clock`, `model`, `gateway`, `executable`, `issuer`, `workspace`, `script_directory`, `concurrency`, `path_prefix`, `sync_key`, `computer_bridge`, `canonical_bridge`, `pdf_bridge` and `composio`. `Server` has `get`, `post`, `post_expect`, `get_expect`, `call`, `wait_get`, `wait_row`, `wait_status`, `restart`, `restart_with`, `shutdown`, `crash`, `start_stopped`, `edit_database`, `refresh_machines` and `output`. `TestRoot::start_error_beside` runs a failing start on the caller's slot; `server::start_pair` starts two servers on both slots at once. |
| `http` | `Reply` (status and JSON), `SseReader`, and `SplitRequest` for a request whose last body bytes are held back. |
| `wait` | `until`, `until_within`, `until_value` (the timeout shows the last value) and `never`. |
| `db` | `Inspector` (read-only, works while the server runs) and `Fixture` (writable, only for seeding the API cannot do and for fault triggers). |
| `workspace` | `TempWorkspace`. |
| `model` | `ScriptedModel`: an OpenAI-compatible model that tests script with `push(Reply)` or `respond(closure)`. `Reply` covers `text`, `deltas`, `tool`, `tool_call`, `tool_calls`, `status`, `hold`, `.usage`, `.delay` and `.finish_reason`. It also serves `catalog` and `fail_catalog`, and records requests through `requests`, `calls`, `wait_calls` and `held`. |
| `fakes` | Paths to `Fake::{Codex, Claude, Acp, Gateway}`, `install` (copy a peer to delete or replace it), and `native_log`, `acp_log`, `gateway_log`, `turns`. |
| `issuer` | `SessionIssuer` records MCP session mints and revocations and issues the fixed token `SESSION_TOKEN`. |
| `providers` | `create_instance`, `fake_instance`, and `AgentSpec` (agent bound to an instance) through `/api/provider-instances` and `/api/agents`. |
| `proxy` | `LossyProxy` forwards, records, loses replies (`drop_next`), goes offline (`set_offline`) and retargets after a restart. |
| `cli` | `Script` runs `minarbot-server script ...` and checks that the JSON `exitCode` equals the process status. It has `run`, `run_input`, `run_chunks`, `wait` and `use_url`. |
| `canonical` | `Canonical` sets up the canonical schema, accounts and Clerk RS256 tokens (test-only key) for hosted-mode requests. |
| `bridge` | Closure-backed bridges for computer, canonical, PDF and Composio test options. |
| `protocol`, `image` | `digest_command`, `canonical_json` and `sha256_hex`; PNG builder and dimensions. |
| `parked` | `ParkedModel`: an OpenAI-compatible model whose routing closure answers a request at once or parks it under a key until `finish(key, Answer)`. Use it when a step must stay open while the test changes state. |
| `webhook`, `upload` | `hmac_sha256_hex` and `HeldPost` (last body byte held back); `chunked_post`. |
| `backup` | `download` (`POST /api/backup`), `restore_workspace` (`minarbot-server restore workspace`), `restore_into_new_root` and `workspace_snapshot`. |
| `images`, `mcp_server`, `pdf` | Fake image provider (raw TCP, can lose replies); external streamable-HTTP MCP server with held calls; PDF fixtures and `PdfGate` (fail once, hold a parse, see it abandoned). |
| `desktop`, `vnc` | `DesktopFixture` for `desktop.testRuntime` (launches, commands, failures, held commands); `VncClient`, a WebSocket + RFB 3.8 client. |
| `fake::stdio_mcp` | `StdioServer`: one stdio MCP request per process, for `minarbot-server --mcp-stdio` or extra harness servers. |
| `reap` | Dropping the last `TestRoot` kills every process whose executable or working directory is inside it (peers left behind by a killed server). |
| `gated` | `GatedModel`: an OpenAI-compatible model whose `Answer` can wait for a `Gate`, park the request for the test (`Answer::Park`, `parked`), hang, or stream chunks with gates between them. Extra JSON routes (`route`) serve, for example, image generation. |
| `stub` | `Stub`: a raw HTTP/1.1 fake for third-party APIs (sync or async handlers, `StubReply::Hang` and `Drop` for lost results, WebSocket upgrades through `with_socket`). |
| `platform`, `connector` | `ConnectorFixture` (fake Slack and Telegram with Socket Mode, a fault-injecting script proxy and the connector process); `Connector` runs `--connector-config`, `journal` runs `--connector-journal-config`. |
| `peer` | `PeerBridge`: a computer bridge with a recording peer token issuer and loopback `peer-http`; pair two servers with `server::start_pair` on a multi-thread test runtime. |
| `signing`, `noise` | `hmac_sha256_hex` for signed event deliveries; `noise_png` for large generated images. |

Configuring a model (`ServerConfig::model` or `gateway`) makes `fake-gateway`
the Codex executable, as the Node bridge did, so native Codex tests configure
no model. Agents created with `"runtime": "api"` then use the gateway peer. Since #87, `runtime` on create is otherwise ignored, so bind an
agent to an instance with `AgentSpec::instance` (or edit with
`runtime`/`confirmRuntime` in test mode).

## Worked example

This is `kit::worked_example_reply_survives_restart` in
`crates/minarbot-e2e/tests/e2e/kit.rs`:

```rust
use minarbot_testkit::{
    model::{Reply, ScriptedModel},
    prelude::*,
};

#[tokio::test]
async fn worked_example_reply_survives_restart() -> Result<()> {
    let model = ScriptedModel::start().await?;
    model.respond(|request| Reply::text(format!("reply:{}", request.last_text())));
    let root = TestRoot::new()?;
    let mut app = root.start(ServerConfig::new().model(&model)).await?;
    let agent = app
        .post_expect(
            "/api/agents",
            json!({"id": new_id(), "runtime": "api", "name": "A",
            "model": "fixture", "instructions": "Help."}),
            201,
        )
        .await?;
    let turns = format!(
        "/api/agents/{}/turns",
        agent["id"].as_str().unwrap_or_default()
    );
    let turn = new_id();
    app.post_expect(&turns, json!({"id": turn, "text": "hello"}), 202)
        .await?;
    let done = app.wait_status(&turns, &turn, "done").await?;
    ensure!(done["reply"] == "reply:hello", "reply: {done}");
    app.restart().await?;
    ensure!(
        app.row(&turns, &turn).await?.ok_or("lost")?["reply"] == "reply:hello",
        "restart"
    );
    ensure!(model.calls() == 1, "restart replayed the turn");
    Ok(())
}
```

For native harness turns, install `Fake::Codex` or `Fake::Claude`, pass them
with `ServerConfig::executable`, script the turn with a prompt keyword
(table below), and read `fake-log.jsonl` with `fakes::native_log`. See
`crates/minarbot-e2e/tests/e2e/harness.rs`.

## Fake peers

The peers are faithful Rust ports of the former Node fixtures; their prompt
keywords, log files and assertions are unchanged. Each one fails (exit 1,
message on stderr) when the server breaks a contract it checks. The kit sets
`MINARBOT_HARNESS_CANARY` in every server environment: the native peers fail
when it reaches them, and `fake-acp` logs it. Every item below is implemented
and proven by `crates/minarbot-testkit/tests/fakes.rs` (the ID is in the test
name) and, where listed, end to end by an e2e module.

| ID | `fake-codex` / `fake-claude` (from `fake-harness.mjs`) | E2E |
| --- | --- | --- |
| N1 | `--version` prints `fake-harness 1.0.0`. | `harness::runtime_changes...` |
| N2 | `login status` / `auth status` exit 0 only with `auth.json` / `.credentials.json` in `CODEX_HOME` / `CLAUDE_CONFIG_DIR`. | - |
| N3 | Fails if `OPENAI_API_KEY`, `ANTHROPIC_API_KEY` (except `minarbot-no-key`), `COMPOSIO_API_KEY`, `MINARBOT_SCRIPT_TOKEN`, `NODE_OPTIONS` or the canary is set. | every harness test |
| N4 | Codex `initialize` -> `userAgent: fake`. | harness |
| N5 | `thread/start` and `thread/resume` check cwd, a loopback `/mcp` URL, `bearer_token_env_var`, a token present, `on-request`, `approvals_reviewer: user`, `inherit: none`, and that neither the token nor the provider key is in `shell_environment_policy.set`. | harness |
| N6 | `turn/start` checks thread ID, no network and `workspaceWrite` roots, then logs `additionalContext`. | `native_check...` |
| N7 | Default turn: `Harness ` delta, shell item, `finished` after 100 ms, usage 11/7, `turn/completed`. | `stream_tool_activity...` |
| N8 | `NATIVE_CHECK`: file change plus a check command with output and exit code. | `native_check...` |
| N9 | `FINAL_SECRET` / `FINAL_PROVIDER_SECRET`: the secret is split across deltas. | `authoritative_snapshot...` |
| N10 | `APPROVE` / `APPROVE_SECRET`: command approval; accept -> `finished`, otherwise `denied`. | `durable_approval...` |
| N11 | `NETWORK`, `OUTSIDE_FILE`, `READ_ONLY_WRITE`: permission or file approval; a network grant counts as allowed. | `workspace_write_policy...` |
| N12 | `STREAM_GATE`: text, tool and a 100-character draft, then wait for `stream-release` and give the final answer. `STREAM_CRASH`: buffered tail, then exit 19. | `gated_segments...`, `error_flush...` |
| N13 | `FIRST_PARTIAL_GATE` holds after `Harness ` until `stream-release` exists. | `stream_tool_activity...` |
| N14 | `CRASH` exits 19. | `crash_fails_once...` |
| N15 | `OVERSIZE` writes 1 MiB + 1 byte with no newline. | `bounded_output...` |
| N16 | `HOLD` (without `RESTART_FINISH`) spawns a logged descendant and waits. | `cancel_kills_descendants...` |
| N17 | `turn/interrupt` and Claude `interrupt` log `interrupted` and answer. | `abort_flush...` |
| N18 | `fake-log.jsonl` records each Codex message with `home`/`codexHome`, plus turn, context, decision, gate and descendant entries. | harness |
| N19 | Claude `initialize` stores the hook callbacks and logs `{type: init, resume, callback, context}`. The `--mcp-config` server must be stdio and must not contain the token. | `stream_tool_activity...` |
| N20 | Claude user input produces `system`, `stream_event` deltas, `assistant`/`user` tool messages and a `result` with usage 11/7. | harness |
| N21 | Claude `PreToolUse` hooks for Bash, WebFetch and Write; `allow` -> `finished`, otherwise `denied`. | `durable_approval...` |
| N22 | `Create a deliverable.`: a `Stop` hook; its reason runs a reminder turn in the same session. | `done_check_nudges...` |
| N23 | `WORKSPACE_SHELL_FIXTURE`: native `ls` and loop denied, then `run_shell` allowed and run through the stdio MCP server. | `claude_workspace_shell...` |
| N24 | Claude `interrupt` control request. | - |
| N25 | `--resume <id>` and `--session-id <id>` set the session. | `stream_tool_activity...` |
| N26 | `NATIVE_SETUP_FIXTURE` with `Initial needs:` calls MCP `create_text_file` (expects 403), then `edit_profile` (expects revision 2). | - |

| ID | `fake-acp` (from `fake-acp.mjs` and the goal-judge script) | E2E |
| --- | --- | --- |
| A1 | `--help`, `--version`. | - |
| A2 | `initialize` with auth methods, `loadSession`, HTTP MCP and models. | goals |
| A3 | `session/new`; `session/load` first replays a historic chunk that must not reach the transcript. | - |
| A4 | A prompt without `session/set_mode default` exits 2. | - |
| A5 | Default prompt: text, a pending tool, `session/request_permission` (once, persistent, deny), tool status by choice, `Final answer.`. | - |
| A6 | `FILES`: writing outside and reading through a link must fail; writing and reading inside must succeed, otherwise exit 4. | - |
| A7 | `READONLY`: the write must fail, otherwise exit 5. | - |
| A8 | `CRASH`: partial text, then exit 3. `INVALID`: prints `bad json`. | - |
| A9 | `TREE`: detached child that survives TERM, pid in `child-pid`. `WAIT`: waits. | - |
| A10 | `session/cancel`: token on stderr, `cancel-received` file, prompt ends `cancelled`. | - |
| A11 | Unknown method -> `-32601`. | - |
| A12 | `fake-acp-log.jsonl` with the leaked canary, if any. | goals |
| A13 | Pi RPC: `prompt` and `extension_ui_response` flow, `get_state` and models; `--append-system-prompt` without `--extension` fails. | - |
| A14 | Judge mode (`fs.readTextFile: false`): session `judge`, no MCP servers allowed, `met` verdict. Work prompts with `ACP_READY` answer `ACP_READY`. | `goals::acp_work_and_judge...` |

| ID | `fake-gateway` (from `fake-gateway-harness.mjs`) | E2E |
| --- | --- | --- |
| G1 | `--version`. | - |
| G2 | `thread/start` and `thread/resume` take the MCP URL, token variable, cwd, thread ID and model. | threads, cli |
| G3 | `tools/list` becomes OpenAI tools; text answers -> deltas, `item/completed`, usage and `turn/completed`. | threads |
| G4 | With `MINARBOT_GATEWAY_STREAM=true`, SSE content and tool-call deltas are folded; each content delta is forwarded as it arrives, and streamed text before tool calls becomes its own `agentMessage` item. | `fx_stream`, `fx_dispatch::restart_cancels...` |
| G5 | Tool calls -> `mcpToolCall` items and MCP `tools/call`, with a stable `requestId` filled in when the schema requires one. | questions, agent_create |
| G6 | MCP HTTP 4xx -> tool error to the model; 5xx -> turn error. | agent_create (`escape`) |
| G7 | An "uncertain" MCP error fails the turn. | - |
| G8 | `MINARBOT_TOOL_BUDGET`: then "Tools are now disabled"; tools after that fail the turn. | agent_create (`escape`) |
| G9 | `<minarbot-message>` tags or `role: ` blocks rebuild the transcript; `.fake-gateway-<thread>.json` restores the session on resume. | agent_create (`hold`) |
| G10 | Provider HTTP errors -> `error` notification. | - |
| G11 | `turn/interrupt` aborts the turn with no further output. | threads |
| G12 | `Initial name:` turns offer only `ask_user` and `edit_profile`. | agent_create |
| G13 | `wait_task` timeouts are clamped to 1-30 s; `delegate_task {wait: true}` waits with `wait_task` unless Minarbot answered `status: waiting` (it then resumes the turn itself); `send_message` fans out to recipients. | - |
| G14 | Image tool results are forwarded as an `image_url` user message. | - |
| G15 | Without an MCP session (goal judges), no tools are offered. The Node fixture crashed here. | goals |
| G16 | `model/list` returns the provider's `GET /models` as one page. The Node fixture had none. | cli |
| G17 | Provider requests carry `MINARBOT_PROVIDER_KEY` as a bearer token and the thread's model. | goals |
| G18 | `fake-gateway-log.jsonl`: method, ID, thread and run stage (`tools`, `provider`, `error`). | - |
| G19 | Every extra `mcp_servers.<name>` entry with a `command` (CodeGraph) is mounted as a stdio MCP server: its tools are offered and called next to Minarbot's. Proven end to end only. | `pd_local_projects::built_in_codegraph...` |

When stdin closes, a peer finishes its pending work (timers, gates,
requests) before it exits, as the Node fixtures did.

## Seams

Agent replies always come from fake peers through the server's options.
Stored features call their API directly, without an agent turn. Time comes
from the fixed clock (`FIXED_CLOCK`, or `fixed_clock` per start). Tests use
only temporary directories and loopback ports. Servers get a hermetic `PATH`
(`/usr/bin:/bin`), so host Codex or Claude installations are never found.

## Honest tests

A failing test means a product bug or a named removed behaviour. Skipped,
ignored or loosened tests count as failures. When a ported case hits a
product bug, leave that assertion out of the Rust test, keep the Node case,
and record the bug with the exact assertion in `docs/test-ledger.md`.
Never use `#[ignore]`.
