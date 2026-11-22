# Minarbot Tauri desktop pilot

The [Electron build](../package.json) remains the default during the packaged pilot. The Tauri app imports the saved Electron server list, active origin and usable window bounds on first launch. It stores later changes under `com.minarbot.desktop.tauri`. Both clients can be installed side by side.

Tauri keeps the connect form and server content in distinct webviews. The bundled form can submit a server address. The server webview uses browser permissions and native menu actions, and has zero Tauri capabilities. There is no registered OS handler for `minarbot-connect:`.

On Linux and macOS, the desktop verifies the embedded runtime and starts the normal Minarbot launcher on loopback. It uses `~/.local/share/minarbot` and `~/.config/minarbot/launcher.json` like `install.sh`. An existing healthy install remains owned by its original launcher. A launcher the desktop starts stops when the desktop quits. Windows uses `install.ps1 -NoOpen` with the bundled Linux archive. Each Tauri executable pins the SHA-256 of its bundled runtime.

## Build and release

1. Build the server, launcher and web app. Package the runtime with `cargo xtask release --pack-ci target/release --output artifacts --notes '<notes>'`.
2. Install Electron dependencies with `npm --prefix desktop ci`. On Linux, install the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) including WebKitGTK 4.1. From the repository root, run `cargo xtask desktop --runtime <platform-runtime.tar.gz> --output desktop-output`. Windows uses the Linux x86_64 archive. `cargo xtask release --desktop --tauri-pilot` includes both Linux x86_64 clients. `cargo xtask release --desktop --dry-run` stages both with disposable signing credentials.
3. Set `MINARBOT_TAURI_UPDATER_PUBKEY` to the Tauri signer's public key. Set `TAURI_SIGNING_PRIVATE_KEY` and optional `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` on the signing job. For the release workflow, enable `tauri_pilot` to include every Tauri installer and `tauri-latest-<platform>.json` alongside legacy assets. The drafts continue through `cargo xtask release --sign-ci`. Run the release matrix for each published version while the pilot is active.
4. Verify `latest.json.sig` against the existing Minarbot release key and check the Tauri installer SHA-256 against its `assets` entry before the one-time install. Keep the previous Electron installer and feed for rollback. Back up the dedicated Tauri updater key and password separately from the server key. For key rotation, deliver a verified installer with the replacement public key and rehearse migration from a build pinned to the prior key before changing the main feed.

### Fixture builds

Generate a disposable key in a private directory with `npx --yes @tauri-apps/cli@2.12.6 signer generate --ci --write-keys <fixture.key>`. Build version N and N+1 with the same fixture key.

```sh
cargo xtask desktop --tauri-only --runtime <runtime.tar.gz> --output <output> \
  --version <fixture-version> --fixture-key <fixture.key> \
  --fixture-feed http://127.0.0.1:<feed-port>/ --fixture-port <server-port>
```

Use separate output directories. Serve the generated feed and artifact from the feed port. The configured fixture port overrides fresh Unix installs. On Windows WSL uses port 4317 inside the disposable VM. Set `HOME` to the disposable Windows profile before launch. Set `MINARBOT_DESKTOP_FIXTURE_HOME` equal to the disposable `HOME` when launching the fixture.

### Packaged acceptance matrix

Run in clean users or snapshots with disposable data and keys. For each OS, record the OS and webview versions and save the version N installer for rollback.

- **Linux / WebKitGTK**. Copy N into a clean `Applications` directory and set executable permission. Launch under an isolated D-Bus session and Xvfb. Confirm onboarding opens, `/api/environment` answers on the fixture port, and the launcher selection and database appear in the expected paths. Relaunch and confirm the same server identity. Choose a different server via the connect form, test a redirect and a non-HTTP URL, then switch back through Servers. Use native clipboard, menu and notification checks below.
- **macOS / WKWebView**. On Intel and Apple silicon, run `codesign --verify --deep --strict` on the mounted `Minarbot Tauri.app`. Drag the app into Applications and open through Finder. Exercise the system's ad-hoc Gatekeeper flow as needed. Confirm first launch connects locally, and quit/reopen uses the same server identity. Repeat with a preinstalled healthy `install.sh` service and confirm it remains running when Tauri quits. For an update, choose **Open Disk Image**, quit, replace the app in Applications, and reopen. Try a bad signature and substituted DMG and confirm neither opens. Restore the N app and confirm saved servers and local data still open.
- **Windows / WebView2 and WSL 2**. On Windows 11 x64, install N through NSIS as a standard user. Confirm WebView2 boots, and verify the clean profile reports WSL missing before Ubuntu is installed. Run `wsl --install -d Ubuntu`, complete Ubuntu user setup, and test the missing dependencies message before installing `curl python3 openssl`. Retry **Servers > Start WSL Server** and verify localhost forwarding and the launcher. Repeat with an already running WSL install, its server stopped, and with a different program holding the forwarded port. Verify an altered archive fails the digest check. Use **Restart to Update**, then reinstall N on a disposable snapshot and verify settings and WSL data persist.

On each target, seed Electron's profile with two server origins and normal window bounds. Launch Tauri twice, alter its saved servers, and reopen Electron to confirm the original profile still works. Launch a second Tauri process and confirm it focuses the running window. Test keyboard submit and a stale probe completing after a newer connection. Verify same-origin links, external HTTP(S) links, blocked cross-origin redirects and server content with zero Tauri permissions. In the app, copy text, paste plain text and an image into the composer, and use Computer clipboard. Test right-click edit/link/image actions. Request notification permission from the foreground app and verify clicking a notification focuses the original window and conversation. Confirm framed or unrelated origins cannot request shell privileges.

For the two-release rehearsal, update Electron N to N+1 from its existing feed. Update Tauri N to N+1 using its fixture feed. Test **Later**, interrupted download, missing artifact, wrong signature, modified artifact, a signed N payload advertised as N+1, and a lower version. Confirm N stays installed on rejection. On Linux, simulate an unwritable installation directory and verify the old AppImage still launches. After a successful update, quit and restore the sibling `Minarbot.previous.AppImage`. Confirm the previous build opens the same data. Keep Electron shipping until the full matrix passes.
