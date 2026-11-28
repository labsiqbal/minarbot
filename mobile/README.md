# Minarbot for phones

`mobile/tauri` is the Android app (iOS follows). It never runs a server: the owner enters the HTTPS address of a Minarbot server on their tailnet, and the app shows that server's web client.

It is a sibling of `desktop/tauri`, not a mobile target inside it. The desktop app is built around a local launcher, menus, an updater, several windows and WebKitGTK hooks, none of which exist on a phone. Gating all of that behind `#[cfg(desktop)]` would touch most of its `main.rs`. The phone crate shares what is the same instead:

- `desktop/tauri/src/connection.rs` (origin rules, server probe), compiled in through `#[path]`, tests included.
- `desktop/tauri/ui` (the connect form), as `frontendDist`.
- `desktop/tauri/icons/icon.png` for the launcher icons.

## How it works

- One webview, label `main`. It opens the bundled connect form, then navigates to the server.
- The form calls `connect_server`, the only command, allowed by `capabilities/connect.json`. That capability has no `remote` entry, so it applies only to the bundled origin; the server page has zero Tauri capabilities.
- Navigation is limited to the bundled form and the connected server's origin. Other HTTP(S) links open in the system browser.
- Addresses must be HTTPS. Debug builds also accept plain HTTP on loopback, for a server on the development host reached through `adb reverse`.
- The last server is remembered in the app's config directory and opened once the form has loaded (Android drops a navigation that starts earlier). If it cannot be reached, the form shows the error with the address filled in.
- On resume, the app checks the server. If it is gone, the form shows why; after a failure, the next resume tries again. If the server is there, the page gets a `visibilitychange` event so its polls catch up.
- Android 15 draws apps edge to edge, and the system WebView may expose no safe-area insets, so `MainActivity` pads the webview by the status bar, gesture bar, cutout and keyboard. The app paints the bars with the colour at the top of the page and picks contrasting icons: after each page load, after each route change, and when the page changes its theme. For the theme, the server page may call `MinarbotBars.changed()`, a JavaScript interface that only asks Rust to read the page colour again.
- Back goes to the web client first (`minarbot:back`, see `crates/web/README.md`): it closes the open menu, sheet or dialog, or steps back through the client's own history. Only when the client has nothing to go back to does the webview go back, which returns to the connect form; at the form, Android closes the app.
- Rust logs go to logcat through `tauri-plugin-log` (`adb logcat | grep -i minarbot`).

## Build a debug APK

Needs JDK 17, the Android SDK (platform 36, build-tools, NDK) and the Rust Android targets:

```sh
rustup target add aarch64-linux-android x86_64-linux-android
cargo xtask android                    # phone (arm64)
cargo xtask android --target x86_64    # emulator
```

`ANDROID_HOME` defaults to `~/Android/Sdk` and `NDK_HOME` to the newest NDK under it. The command installs the same `tauri-cli` the desktop build pins, into `target/tauri-cli`. The APK lands in `mobile/tauri/gen/android/app/build/outputs/apk/universal/debug/`. On a shared host, run it through `~/.cache/minarbot-cargo-slot.sh`.

The commands it runs, by hand:

```sh
cargo install --locked tauri-cli --version 2.12.6 --root target/tauri-cli
cd mobile/tauri
ANDROID_HOME=~/Android/Sdk NDK_HOME=~/Android/Sdk/ndk/<version> \
  ../../target/tauri-cli/bin/cargo-tauri android build --debug --apk --ci --target x86_64
```

### Try it on an emulator

```sh
avdmanager create avd -n MinarbotSpike -k "system-images;android-35;google_apis;x86_64" -d pixel_7
emulator -avd MinarbotSpike &
adb install -r mobile/tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk
```

Run a server on the host, for example `PORT=4339 MINARBOT_DATA_DIR=<temp dir> target/debug/minarbot-server`, then forward the port and connect to `http://127.0.0.1:4339`:

```sh
adb reverse tcp:4339 tcp:4339
```

A Minarbot server answers only on its own address (`http://127.0.0.1:<port>`) and its HTTPS allowed origins, so the emulator's `10.0.2.2` alias gets a 403. A real phone uses the server's tailnet HTTPS address, which the server lists in `MINARBOT_ALLOWED_ORIGINS`, and pairs like any other device.

## The Android project

`gen/android` was made by `cargo tauri android init` and is committed with these changes:

- `compileSdk` and `targetSdk` 36 (the SDK installed on the build host) instead of the template's 37.
- Minarbot launcher icons in `app/src/main/res`.
- Push: `firebase-messaging`, the Google services Gradle plugin applied only when `app/google-services.json` exists, `POST_NOTIFICATIONS`, and `PushService` in the manifest.
- `NativePlugin.kt` (push and system bar colours) and `PushService.kt` next to `MainActivity.kt`.

If you run `init` again, put these back.

## Push

The phone registers its FCM token with the server's `POST /api/push/devices` (contract on labsiqbal/minarbot#109). It sends the request from inside the server page, so it carries the page's paired-device session. The plugin has no JavaScript commands and no permissions; only Rust calls it.

- `NativePlugin.register` returns the FCM token and the device model. Without a Firebase config it returns a reason instead, and logcat shows `Push: registration skipped: This build has no Firebase config (google-services.json).` The app works normally without push.
- A rotated token (`PushService.onNewToken`) is sent to the server the phone shows, and the old device is deleted.
- Tapping a notification opens its `path` (for example `/agents/<id>/conversations/<thread>`) inside the shown web client (`minarbot:open`), without a page load; a client without that event loads the route. Routes that leave the server are ignored.
- On Android 13 and later, the app asks for notification permission once Firebase is configured.

Existing Tauri push plugins were checked in November 2026 and not used: `tauri-plugin-mobile-push` 0.1.4 never reaches its Android code, `tauri-plugin-notifications` 0.5.0-rc.17 calls Firebase unguarded from a permission callback (a likely crash without Firebase), `tauri-plugin-fcm` 0.2.1 has no tap events, `tauri-plugin-firebase` 0.1.0-alpha.8 is guarded but brings Analytics and Crashlytics, and `tauri-plugin-push-notifications` 0.1.0 has one author and drops token refreshes. The in-repo plugin is about 100 lines of Kotlin.

### What the owner adds for Android push

1. Create a Firebase project and add an Android app with package name `com.minarbot.mobile`.
2. Download its `google-services.json` to `mobile/tauri/gen/android/app/google-services.json`. Git ignores it; never commit it.
3. Rebuild with `cargo xtask android` and reinstall. Logcat shows `Push: token ready` instead of `registration skipped`.
4. Give the server FCM credentials (a service account for the FCM HTTP v1 API), as the server side of #109 describes.

### iOS later

- An Apple Developer account, for the push entitlement, an APNs key and TestFlight.
- A Mac with Xcode to run `cargo tauri ios init` and build.
- A Swift `NativePlugin` that returns the APNs device token as hex from `didRegisterForRemoteNotificationsWithDeviceToken`; `push.rs` already sends `platform: "ios"` on iOS.
