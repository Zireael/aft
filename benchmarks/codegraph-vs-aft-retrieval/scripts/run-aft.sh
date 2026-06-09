#!/usr/bin/env bash
set -euo pipefail
exec bun run src/cli.ts \
  --driver aft \
  --corpus "${RETRIEVAL_CORPUS:-${TARGET:-opencode-aft}}" \
  --binary "${AFT_BINARY:-/workspace/target/release/aft}" \
  --out-dir "${RETRIEVAL_OUT_DIR:-results}" \
  --topK "${TOP_K:-10}" \
  --timeout-ms "${RETRIEVAL_TIMEOUT_MS:-600000}" \
  ${PREPARE_TARGET:+--prepare-target}
