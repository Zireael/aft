#!/usr/bin/env bash
set -euo pipefail

DRIVER="${CODEGRAPH_REPLICATION_DRIVER:-aft}"
CORPUS="${CODEGRAPH_REPLICATION_CORPUS:-codegraph}"
CODEBASE="${CODEGRAPH_REPLICATION_CODEBASE:-/workspace}"
OUT_DIR="${CODEGRAPH_REPLICATION_OUT_DIR:-results}"
TOP_K="${CODEGRAPH_REPLICATION_TOP_K:-10}"
RUNS="${CODEGRAPH_REPLICATION_RUNS:-1}"
READY_TIMEOUT_MS="${CODEGRAPH_REPLICATION_READY_TIMEOUT_MS:-600000}"

mkdir -p "$OUT_DIR"
exec bun run src/cli.ts \
  --driver "$DRIVER" \
  --corpus "$CORPUS" \
  --codebase "$CODEBASE" \
  --binary /workspace/target/release/aft \
  --out-dir "$OUT_DIR" \
  --topK "$TOP_K" \
  --runs "$RUNS" \
  --ready-timeout-ms "$READY_TIMEOUT_MS" \
  ${CODEGRAPH_REPLICATION_VERBOSE:+--verbose}
