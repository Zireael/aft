#!/usr/bin/env bash
set -euo pipefail

MODE="${AFT_SEARCH_MODE:-in-tree}"
OUT="${AFT_SEARCH_OUT:-results/aft-search-docker.json}"
BINARY="${AFT_BINARY:-/workspace/target/release/aft}"
PROJECT_ROOT="${AFT_PROJECT_ROOT:-/workspace}"
READY_TIMEOUT="${AFT_READY_TIMEOUT:-300}"

mkdir -p "$(dirname "$OUT")"

case "$MODE" in
  in-tree)
    exec python3 run.py \
      --binary "$BINARY" \
      --project-root "$PROJECT_ROOT" \
      --out "$OUT" \
      --ready-timeout "$READY_TIMEOUT"
    ;;
  external)
    if [ "${AFT_SEARCH_SETUP_CORPUS:-1}" = "1" ]; then
      python3 setup_corpus.py
    fi
    exec python3 run_external.py \
      --binary "$BINARY" \
      --results-dir results \
      --out "$OUT" \
      --ready-timeout "${AFT_READY_TIMEOUT:-600}" \
      ${AFT_SEARCH_ALLOW_PARTIAL:+--allow-partial}
    ;;
  *)
    echo "Unknown AFT_SEARCH_MODE=$MODE (expected in-tree|external)" >&2
    exit 2
    ;;
esac
