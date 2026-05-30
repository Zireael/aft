#!/usr/bin/env bash
# Build and run Linux E2E tests in Docker.
# Uses aimock + OpenCode to test the full AFT plugin stack.
#
# Usage:
#   ./tests/docker/run-linux-test.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

echo "Building Linux x64 E2E test image..."
echo "(Installs OpenCode + locally packed AFT plugin/bridge + aimock)"
echo ""

AFT_LINUX_FIXTURE="$SCRIPT_DIR/fixtures/aft-linux-x64"
if [ ! -x "$AFT_LINUX_FIXTURE" ]; then
    cat >&2 <<EOF
Missing executable Docker E2E fixture: $AFT_LINUX_FIXTURE
Build it first, for example:
  docker build -t aft-build-linux -f tests/docker/Dockerfile.build-linux .
  docker cp \$(docker create aft-build-linux true):/build/target/release/aft "$AFT_LINUX_FIXTURE"
  chmod +x "$AFT_LINUX_FIXTURE"
EOF
    exit 2
fi

PACK_DIR="$SCRIPT_DIR/fixtures/npm-packs"
rm -rf "$PACK_DIR"
mkdir -p "$PACK_DIR"
cleanup_packs() {
    rm -rf "$PACK_DIR"
}
trap cleanup_packs EXIT

npm pack "$REPO_ROOT/packages/aft-bridge" --pack-destination "$PACK_DIR" >/dev/null
npm pack "$REPO_ROOT/packages/opencode-plugin" --pack-destination "$PACK_DIR" >/dev/null

docker build \
    --platform linux/amd64 \
    -f "$SCRIPT_DIR/Dockerfile.linux-x64" \
    -t aft-e2e-linux-x64 \
    "$REPO_ROOT"

echo ""
echo "Running E2E tests..."
docker run --rm --platform linux/amd64 aft-e2e-linux-x64
