# Hosted Hub runtime and deployment

Run `minarbot-server --hub` with `MINARBOT_HUB_CONFIG` set to an explicit JSON
file. The binary binds loopback on port 4318 by default. Keep the Hub database
and key files private, separate from workspace and canonical databases. The
Hub holds an exclusive writer lock and checks the database marker and migration
checksum before serving.

Use these configuration fields.

| Field | Value |
| --- | --- |
| `domain`, `accountsHost` | Project domain and exact `accounts.<domain>` central hostname |
| `dataPath`, `mailboxKeyPath` | Dedicated SQLite path and a file with at least 32 bytes of private key material for mailbox lookup |
| `identity` | `mode: "clerk"`, exact HTTPS `issuer`, pinned public `jwksPath`, and optional `audience` |
| `nodeLink.signingKeyPath` | Ed25519 PKCS#8 private key used for signed Hub frames |
| `web.distPath` | Packaged `dist/hub` directory, with the Leptos `dist/_leptos` sibling built by `cargo xtask leptos` |
| `port`, `trustedGatewayAddresses` | Optional listener port and exact proxy source IPs allowed to forward the browser host |

Terminate TLS at the operator proxy. Preserve exact accounts and project Host
headers. Forward node connections to the central host with the original Host.
Only a configured browser proxy may use `X-Forwarded-Host`. The pinned JWT
verifier makes no identity network calls. The browser can continue a valid
session. Configure the production identity provider before exposing sign-in.
Synthetic identity and clock fixtures use private loopback test instances.

## Account and database maintenance

Stop the hosted listener before running these commands with the same config.

```sh
minarbot-server --hub admit ISSUER SUBJECT
minarbot-server --hub account-status ID active
minarbot-server --hub account-status ID blocked
minarbot-server --hub backup PATH
```

The backup command creates a new private snapshot. Supply a separate config
pointing to a restore copy. Run `minarbot-server --hub fence-restore`, inspect
the copy, then use `minarbot-server --hub activate GENERATION` with the returned
generation. Fencing invalidates sessions and leases, holds pending invitations,
and marks unfinished work unknown. Operators manage off-host snapshot encryption
and recover stale writer locks with the listener stopped.

## Code and test boundaries

- `crates/minarbot-server/src/hub/` serves account and project HTTP routes,
  exact-host sessions, signed enrollment and transport, grants, outbox and
  replay. `crates/minarbot-server/src/computer/hub.rs` owns the member client
  and its local execution and effect fences.
- `crates/web/src/hub.rs` and `crates/web/src/hub/` own the hosted Leptos UI.
  The hosted process serves the packaged browser assets. The current React
  selector and its assets are covered by the separate UI retirement work.
- `app/hub-link.test.ts`, the hosted gateway suites, and native Hub tests cover
  authentication, tenant scope, grants, replay, session and database recovery.
  See the [test ledger](test-ledger.md#67-hosted-hub-test-migration).
- `deploy/email-routing/worker.mjs` runs in Cloudflare Email Routing.
  `app/t3/pi-harness-extension.mjs` runs in the Pi host for MCP registration
  and local tool approval. Electron packaging stays in use pending #66.

## Stage B consumer check

Repository-wide searches covered source imports and executable invocations in
`app/`, `crates/xtask/`, Electron and Tauri under `desktop/`, install scripts,
workflows, `deploy/`, and the docs. The Node Hub entrypoint and its exported
`transferOwner`, `archiveSection`, and `deleteProject` had no mounted product
route or package consumer. Remaining authority tests exercise current Rust
handlers and persisted schema constraints.

`@clerk/react`, `@napi-rs/keyring`, `@anthropic-ai/claude-agent-sdk`, and
`sanitize-html` had no import consumers. The removed sanitizer reference in
Vite only named a chunk. `@types/sanitize-html` had no type consumer.
`@clerk/backend` still feeds canonical fixtures. MCP SDK and `ws` remain in
transport and connector suites. The Anthropic SDK entry in T3 provenance
describes historical adapter source. Native `minarbot-server skills`,
`script`, and `restore` replace the npm CLI aliases. Release staging drops
their shell shim and retains the Python shared-data worker and notices.
