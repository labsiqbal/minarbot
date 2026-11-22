# Minarbot

Minarbot is a self-hosted workspace where you work with a team of AI agents. You chat with each agent, group agents into projects, give them tools and their own computer, and schedule work that runs while you are away. Agents can run on several of your machines and talk to each other across them.

Minarbot is free and open source under the [Apache License 2.0](LICENSE).

> **Status:** early and moving fast. It runs every day on Linux machines. macOS uses an ad-hoc signed desktop build. Windows runs the Linux server inside WSL 2.

## Features

**Agents and chat**
- Create agents from templates, give each one an avatar, instructions and its own working folder.
- Streaming replies with a collapsible "Worked for" summary of every step, side threads, question and approval cards, and compaction for long conversations.
- Steer a running agent, stop it, queue messages, attach files and images, or record voice.
- **Projects** group agents around one piece of work. **Sections** inside a project are group chats for different kinds of work, such as design or backend.

**Agent engines**
- Coding harnesses: Codex, Claude Code, and ACP engines (Hermes, Antigravity, Grok, Pi, Gemini, OpenCode). First run picks up Codex or Claude Code already signed in on the machine, so no API key is needed.
- Plain model APIs: OpenAI-compatible, Anthropic and Gemini accounts, managed in Settings.
- Shared base instructions and a self-check before an agent reports a task as done.
- Web search and fetch for every agent, through a search backend you choose (for example a local SearXNG).

**Tools**
- **Computer:** an optional per-agent Linux desktop with its own Chrome profile, a live viewer and takeover. See [Computer setup](#computer-optional).
- Skills (folders, zip files and skill packs), MCP client and MCP server, connected apps through Composio, and encrypted environment secrets per project.
- Memory, shared data, hosted sites and HTML preview, image generation, speech and transcription, and PDF text.
- Files, changes and source control in the side panel.

**Automation**
- Routines: run an agent once, daily, on weekdays, weekly, every N minutes or on a cron schedule, bound to a project, with results posted to a section or a chat.
- Workflows: a graph editor with agent, model, condition, approval, wait, parallel and output steps, plus an inbox and webhooks.
- Slack and Telegram integrations.
- A run inspector, and a "Needs you" list with notifications for agents waiting on you.

**Machines**
- Pair several machines. Each runs its own agents, and agents talk across machines.
- One-click updates for every machine from GitHub Releases, verified against a signed manifest, with rollback.
- Installable as a mobile web app (PWA), backup and restore, usage and host metrics, and color themes.

## Install

Minarbot installs a local server and opens it in your browser.

### Linux and macOS

Install with `curl`, Python 3, and OpenSSL 3. On macOS, Homebrew provides OpenSSL 3 with `brew install openssl@3`.

```sh
curl -fsSL https://raw.githubusercontent.com/labsiqbal/minarbot/main/scripts/install.sh | sh
```

To install and pair, copy the command from **Machines > Add machine > Install Minarbot** on your existing installation. Set up reachable HTTPS addresses on both machines first, and put each machine's own address first in `MINARBOT_ALLOWED_ORIGINS` in its `launcher.env`.

For the desktop app, download the AppImage for Linux or the DMG for macOS. If macOS blocks Minarbot on first launch, choose **System Settings > Privacy & Security > Open Anyway**. [Releases](https://github.com/labsiqbal/minarbot/releases/latest)

### Windows 11

Install from PowerShell. If Ubuntu is missing, run `wsl --install -d Ubuntu` in Administrator PowerShell and open Ubuntu once to finish its setup. In Ubuntu, install `curl`, `python3`, and `openssl` if needed.

```powershell
irm https://raw.githubusercontent.com/labsiqbal/minarbot/main/scripts/install.ps1 | iex
```

For the desktop app, download the Windows installer. If SmartScreen blocks it, choose **More info > Run anyway**. [Releases](https://github.com/labsiqbal/minarbot/releases/latest)

### After install

Open <http://127.0.0.1:4317>. Sign in to your preferred agent engine or add an API key in **Settings > Provider accounts**. Use **Settings > Updates** for server updates.

If your data directory was created by Minarbot 0.14.0 or earlier, move it aside before installing. This release requires a fresh data directory.

> **Security:** the local server has no login. Anyone who can reach its port can use your agents, files and provider accounts. Keep it on `127.0.0.1` or a private network such as Tailscale. Add extra origins with `MINARBOT_ALLOWED_ORIGINS` in `launcher.env`.

### Computer (optional)

Computer gives an agent its own Linux desktop with Chrome. Computer is unavailable on macOS.

Choose **Set up Computer** in **Settings > Computer** or on the first-run screen. Minarbot downloads the desktop image and verifies its digest against the signed release manifest. If no container runtime is available, the same screen lists missing programs and offers to install them when supported.

Once the device is ready, turn on **Computer** in the agent's settings. By default, two Computers can run at once per machine. Each stops after 15 idle minutes. For headless setup, `MINARBOT_AGENT_DESKTOP=1` enables setup automatically and `MINARBOT_AGENT_DESKTOP=0` disables Computer.

Enable **Always-on Computer** in the agent’s settings to start its Computer, skip idle shutdown and start it again when the server starts. It uses one of the same concurrent Computer slots. An explicit stop lasts until you start that Computer again or restart the server.

Chrome uses the same profile after each start. For encryption at rest on Linux, put `agent-desktops` on an encrypted filesystem such as a LUKS volume. Set `MINARBOT_DESKTOP_DATA_DIR` to an existing absolute subdirectory inside that volume and mount it before starting Minarbot. To keep existing logins, move the `agent-desktops` directory into that location while Minarbot is stopped. Minarbot uses Chrome’s basic password store. On an unencrypted filesystem, profile data, including cookies and local storage, has filesystem permissions only. Volume encryption covers the desktop profile and home while the volume is locked. Browser data is readable by the desktop while it runs. Provision encryption separately for backups and the server database.

### Library docs

Minarbot reads manifests and lockfiles in the agent's current working folder. It adds a short summary of detected versions to the turn and exposes `project_stack` and `library_docs` in API, Codex, Claude Code, and ACP turns. `library_docs` uses the detected version if there is exactly one. Use `version` to select among multiple versions. The tool labels the source and returns `source_version: null` when the source cannot verify its version.

Library docs are on by default. In the agent profile's **Advanced** tab, turn off **Library docs** to disable both tools and the stack summary. Turning off **Web access** also disables them.

**Settings → General → Library docs** shows the connection and key status. Anonymous lookups use Context7's lower rate limits. To use a key, add a Vault **Secret** named `context7-api-key`, scoped to `https://context7.com`, with no agent grants. The server sends it only as a bearer header to Context7. Delete the Vault entry to return to anonymous use.

The hosted API receives the library name or its public Context7 ID, the requested version when available, and a short public API topic. Minarbot keeps manifest contents, file paths, project metadata, conversation history and project credentials out of these requests. Use public identifiers and keywords in tool arguments. Context7 may retain lookup queries under its data privacy policy. [Context7 data privacy](https://context7.com/docs/security/data-privacy)

Minarbot calls the documented v2 search and context endpoints. The reference MCP server is MIT-licensed and is used as API documentation. The hosted service has its own terms and rate limits. Lookups run on demand, respect `Retry-After`, and use official sources after an error or index/version miss. Successful Context7 excerpts are cached locally for 24 hours, official excerpts for one hour. Documentation excerpts remain subject to the original sources' terms. [API guide](https://context7.com/docs/api-guide) · [MCP client reference](https://github.com/upstash/context7/blob/b1e3611e32a8b1369ae526cacdf335ddfd6e5fbb/packages/mcp/src/lib/api.ts) · [MIT license](https://github.com/upstash/context7/blob/b1e3611e32a8b1369ae526cacdf335ddfd6e5fbb/LICENSE) · [Upstash terms](https://upstash.com/legal/terms)

### Routine templates

Choose **Routines** → **New from template**. Register the URL from **Webhook settings** with the source. Paste the source’s signing secret into Minarbot to enable delivery. Schedule uses the routine’s time and timezone settings.

| Template | Source configuration |
| --- | --- |
| Slack message | Events API message subscriptions. Save the app’s signing secret in Minarbot before registering the URL. Bot messages are ignored. |
| Linear issue | Issue webhooks and their signing secret. |
| Sentry alert | Internal integration issue alert action and the integration’s client secret. |
| GitHub issue | Repository webhook with content type \`application/json\` and the Issues event. Use the same secret on both sides. |

External services need a reachable HTTPS endpoint. Use a relay or reverse proxy that forwards only the assigned webhook path and preserves the raw body and signature headers. Keep the Minarbot UI and other API routes private. Signing secrets are encrypted with a host-local key. Re-enter secrets after moving the database to another host. Event payloads become reference data in the routine’s conversation and go to its model provider. **Pause** stops new deliveries from starting runs.

## Run from source

You need Node.js 22.23.4 or later, npm, and a Rust toolchain.

```sh
git clone https://github.com/labsiqbal/minarbot.git
cd minarbot/app
npm ci
npm run build
cd ..
cargo run --release -p minarbot-server
```

Open <http://127.0.0.1:4317>. Set `PORT` to use another port and `MINARBOT_DATA_DIR` to keep data somewhere other than `~/.local/share/minarbot/new-app`. Rebuild the UI with `npm run build` in `app/` after changing it.

## How it fits together

| Path | What it is |
|---|---|
| `crates/minarbot-server` | Rust server (Tokio, Axum, SQLite): agents, turns, engines, tools, automation, machines |
| `crates/minarbot-launcher` | Rust launcher that starts the server, installs updates and rolls back |
| `app/src` | React and TypeScript UI, built with Vite |
| `app/*.test.ts` | Node test suite that drives the Rust server end to end |
| `desktop` | Electron desktop app (AppImage, DMG, NSIS) |
| `crates/xtask` | Release, packaging, smoke tests and gates |
| `scripts` | Install scripts and release budgets |

## Release tooling

Run from a clean checkout at the release commit. Install dependencies with `(cd app && npm ci)` and `(cd desktop && npm ci)`. Keep local branches named in the shipped issues' claim comments until after the release.

```sh
cargo xtask release --releases ~/.local/share/minarbot-build/releases \
  --issues 58 --desktop --no-image --notes "User-facing release notes"
```

Release runs app lint, the React and Leptos builds, and app tests. It runs Cargo format, Clippy, tests and the locked release build before the resource and production environment gates. Use `--no-github` for the local feed. Omit `--no-image` and set `MINARBOT_GHCR_USER` and `MINARBOT_GHCR_TOKEN` to publish the Computer image with the release.

Retry a completed local build with `cargo xtask release --github-only <release-id> --releases <directory>`. CI packages runtimes with `--pack-ci <binary-directory> --output <directory> --notes <notes>` and uploads a draft with `--draft-ci <directory> --notes <notes>`. Finalize that draft using `cargo xtask release --sign-ci <version>`. Signing uses `--signing-key`, `MINARBOT_RELEASE_SIGNING_KEY`, or the default `~/.config/minarbot-release/signing-ed25519.pem`.

For artifact inspection, run `cargo xtask release --dry-run --releases "$(mktemp -d)" --notes "Dry run"` after building `app/dist`, running `cargo xtask leptos`, and building `target/release`. This stages the release and unsigned manifest locally. Pass `--desktop` for Electron and Tauri AppImages and their updater metadata. See the [Tauri pilot](desktop/tauri/README.md) for build prerequisites, signing and the packaged acceptance matrix. `--issues` reads issue comments and checks their branch tips.

Use `cargo xtask install-smoke target/debug` to exercise the installer against a local fixture. Check a served build with `cargo xtask check-served <origin> --present <text> --expect-version <version>`.

## Contributing

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) for setup, checks and the pull request flow. Report security problems as described in [SECURITY.md](SECURITY.md), not in public issues.

## License

Minarbot's own code is licensed under the [Apache License 2.0](LICENSE). Some parts are adapted from MIT-licensed projects (T3 Code, Hermes Agent and Bot HQ), and the Computer viewer uses noVNC. Their licenses live next to that code in `NOTICE` and `*.LICENSE` files, with `provenance.json` records of what was adapted.
