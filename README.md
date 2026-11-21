# Minarbot

Minarbot is a self-hosted workspace where you work with a team of AI agents. You chat with each agent, group agents into projects, give them tools and their own computer, and schedule work that runs while you are away. Agents can run on several of your machines and talk to each other across them.

Minarbot is free and open source under the [Apache License 2.0](LICENSE).

> **Status:** early and moving fast. It runs every day on two Linux machines, but expect rough edges and breaking changes. Today it supports **Linux x86_64 only**. macOS and Windows are planned in [#13](https://github.com/labsiqbal/minarbot/issues/13).

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

Minarbot runs as a local server you open in the browser. A one-line installer is planned in [#13](https://github.com/labsiqbal/minarbot/issues/13). Until then, install a release by hand.

If your data directory was created by Minarbot 0.14.0 or earlier, move it aside before installing. This release requires a fresh data directory.

**Requirements:** Linux x86_64, `curl`, `tar` and `sha256sum`. To use agents, install and sign in to at least one engine (for example [Codex CLI](https://developers.openai.com/codex/cli) or [Claude Code](https://claude.com/product/claude-code)), or add a model API key in Settings after the first start. An agent on the machine's Codex sign-in also uses Codex's own config, such as its MCP servers and rules. Chats from an agent on either sign-in appear in that CLI's history.

1. Download the latest release and check it against `latest.json`:

   ```sh
   mkdir -p ~/minarbot-download && cd ~/minarbot-download
   curl -fsSLO https://github.com/labsiqbal/minarbot/releases/latest/download/latest.json
   archive=$(sed -n 's/.*"archive":"\([^"]*\)".*/\1/p' latest.json)
   id=$(sed -n 's/.*"id":"\([^"]*\)".*/\1/p' latest.json)
   sha=$(sed -n 's/.*"sha256":"\([^"]*\)".*/\1/p' latest.json)
   curl -fsSLO "https://github.com/labsiqbal/minarbot/releases/latest/download/$archive"
   echo "$sha  $archive" | sha256sum -c -
   ```

2. Unpack it and point `current` at it:

   ```sh
   releases=~/.local/share/minarbot/releases
   mkdir -p "$releases/$id" ~/.local/share/minarbot/data ~/.config/minarbot
   tar -xzf "$archive" -C "$releases/$id"
   ln -sfn "$id" "$releases/current"
   ```

3. Write the launcher configuration. The launcher only accepts private files:

   ```sh
   cfg=~/.config/minarbot
   : > "$cfg/launcher.env"
   printf '{"dataDir":"%s","envFile":"%s","releasesDir":"%s","port":4317}\n' \
     ~/.local/share/minarbot/data "$cfg/launcher.env" "$releases" > "$cfg/launcher.json"
   chmod 700 "$cfg" ~/.local/share/minarbot/data
   chmod 600 "$cfg/launcher.json" "$cfg/launcher.env"
   ```

4. Start it, then open <http://127.0.0.1:4317>:

   ```sh
   "$releases/current/bin/minarbot-launcher" --config ~/.config/minarbot/launcher.json
   ```

To keep it running in the background, create `~/.config/systemd/user/minarbot.service`:

```ini
[Unit]
Description=Minarbot
After=network.target

[Service]
ExecStart=%h/.local/share/minarbot/releases/current/bin/minarbot-launcher --config %h/.config/minarbot/launcher.json
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
```

Then run `systemctl --user daemon-reload && systemctl --user enable --now minarbot.service`. Later updates install from inside the app.

> **Security:** the local server has no login. Anyone who can reach its port can use your agents, files and provider accounts. Keep it on `127.0.0.1` or a private network such as Tailscale. Add extra origins with `MINARBOT_ALLOWED_ORIGINS` in `launcher.env`.

### Computer (optional)

Computer gives an agent its own desktop with Chrome. It is off until you set it up on the machine and turn it on for an agent. One-click setup is planned in [#11](https://github.com/labsiqbal/minarbot/issues/11). For now, on Debian, Ubuntu or Mint:

```sh
sudo apt install tigervnc-standalone-server xfwm4 xfdesktop4 xfce4-panel xdotool scrot bubblewrap dbus
```

Also install [Google Chrome](https://www.google.com/chrome/) so that `google-chrome` is on the `PATH`. Add `MINARBOT_AGENT_DESKTOP=1` to `launcher.env`, restart Minarbot, then turn on **Computer** in the agent's settings. By default two computers run at once per machine, and each stops after 15 idle minutes.

Enable **Always-on Computer** in the agent’s settings to start its Computer, skip idle shutdown and start it again when the server starts. It uses one of the same concurrent Computer slots. An explicit stop lasts until you start that Computer again or restart the server.

Chrome uses the same profile after each start. For encryption at rest on Linux, put `agent-desktops` on an encrypted filesystem such as a LUKS volume. Set `MINARBOT_DESKTOP_DATA_DIR` to an existing absolute subdirectory inside that volume and mount it before starting Minarbot. To keep existing logins, move the `agent-desktops` directory into that location while Minarbot is stopped. Minarbot uses Chrome’s basic password store. On an unencrypted filesystem, profile data, including cookies and local storage, has filesystem permissions only. Volume encryption covers the desktop profile and home while the volume is locked. Browser data is readable by the desktop while it runs. Provision encryption separately for backups and the server database.

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
| `desktop` | Electron desktop app (Linux AppImage) |
| `scripts` | Release, install and gate scripts |

## Contributing

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) for setup, checks and the pull request flow. Report security problems as described in [SECURITY.md](SECURITY.md), not in public issues.

## License

Minarbot's own code is licensed under the [Apache License 2.0](LICENSE). Some parts are adapted from MIT-licensed projects (T3 Code, Hermes Agent and Bot HQ), and the Computer viewer uses noVNC. Their licenses live next to that code in `NOTICE` and `*.LICENSE` files, with `provenance.json` records of what was adapted.
