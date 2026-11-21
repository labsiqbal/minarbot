#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
if ! command -v podman >/dev/null || [ "$(podman --remote=false info --format '{{.Host.Security.Rootless}}')" != true ]; then
  echo 'SKIP: rootless Podman is required for the Computer image integration test.'
  exit 77
fi
slot="$HOME/.cache/minarbot-cargo-slot.sh"
if [ ! -x "$slot" ]; then
  slot=env
fi
stage=$(mktemp -d)
tag=localhost/minarbot-computer-test:$(basename "$stage" | tr '[:upper:]' '[:lower:]')
trap 'podman --remote=false image rm "$tag" >/dev/null 2>&1 || true; rm -rf "$stage"' EXIT
"$slot" cargo build -p minarbot-server --bin minarbot-server
cp "${CARGO_TARGET_DIR:-target}/debug/minarbot-server" "$stage/"
cp computer/image/Containerfile "$stage/"
cp app/LICENSE app/agent-delegation.LICENSE app/t3/NOTICE "$stage/"
podman --remote=false build --platform linux/amd64 -f "$stage/Containerfile" -t "$tag" "$stage"
MINARBOT_TEST_COMPUTER_IMAGE="$tag" "$slot" cargo test -p minarbot-server container_desktop_proxy_profile_and_idle -- --ignored
