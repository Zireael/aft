#!/usr/bin/env bash
set -euo pipefail
exec bun run src/cli.ts \
  --driver codegraph \
  --corpus "${RETRIEVAL_CORPUS:-${TARGET:-opencode-aft}}" \
  --out-dir "${RETRIEVAL_OUT_DIR:-results}" \
  --topK "${TOP_K:-10}" \
  --timeout-ms "${RETRIEVAL_TIMEOUT_MS:-600000}" \
  ${PREPARE_TARGET:+--prepare-target}
