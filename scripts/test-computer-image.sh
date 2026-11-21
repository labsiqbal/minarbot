#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
if command -v podman >/dev/null && [ "$(podman --remote=false info --format '{{.Host.Security.Rootless}}')" = true ]; then
  runtime() { podman --remote=false "$@"; }
elif command -v docker >/dev/null; then
  for socket in "${XDG_RUNTIME_DIR:-}/docker.sock" /var/run/docker.sock; do
    if [ -S "$socket" ] && env -u DOCKER_CONTEXT docker --host "unix://$socket" info >/dev/null 2>&1; then
      break
    fi
  done
  runtime() { env -u DOCKER_CONTEXT docker --host "unix://$socket" "$@"; }
fi
if ! command -v runtime >/dev/null || ! runtime info >/dev/null 2>&1; then
  echo 'SKIP: rootless Podman or local Docker is required for the Computer image integration test.'
  exit 77
fi
slot="$HOME/.cache/minarbot-cargo-slot.sh"
if [ ! -x "$slot" ]; then
  slot=env
fi
stage=$(mktemp -d)
tag=localhost/minarbot-computer-test:$(basename "$stage" | tr '[:upper:]' '[:lower:]')
trap 'runtime image rm "$tag" >/dev/null 2>&1 || true; rm -rf "$stage"' EXIT
"$slot" cargo build -p minarbot-server --bin minarbot-server
cp "${CARGO_TARGET_DIR:-target}/debug/minarbot-server" "$stage/"
cp computer/image/Containerfile "$stage/"
cp app/LICENSE app/agent-delegation.LICENSE app/t3/NOTICE crates/minarbot-server/whatsapp-rust.LICENSE "$stage/"
runtime build --platform linux/amd64 -f "$stage/Containerfile" -t "$tag" "$stage"
MINARBOT_TEST_COMPUTER_IMAGE="$tag" "$slot" cargo test -p minarbot-server container_desktop_proxy_profile_and_idle -- --ignored
