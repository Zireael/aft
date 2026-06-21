#!/usr/bin/env bash
# CI Recall Gate — checks benchmark intent_metrics against baseline thresholds.
#
# Usage:
#   bash scripts/ci-recall-gate.sh <baseline.json> <current.json>
#
# Exit codes:
#   0 — no regression detected
#   1 — regression detected (>5% drop in any intent category Recall@10)

set -euo pipefail

BASELINE_FILE="${1:?Usage: ci-recall-gate.sh <baseline.json> <current.json>}"
CURRENT_FILE="${2:?Usage: ci-recall-gate.sh <baseline.json> <current.json>}"

if [ ! -f "$BASELINE_FILE" ]; then
  echo "ERROR: baseline file not found: $BASELINE_FILE"
  exit 1
fi

if [ ! -f "$CURRENT_FILE" ]; then
  echo "ERROR: current run file not found: $CURRENT_FILE"
  exit 1
fi

echo "=== CI Recall Gate ==="
echo "Baseline: $BASELINE_FILE"
echo "Current:  $CURRENT_FILE"
echo "Threshold: 5% drop"
echo ""

node "$(dirname "$0")/ci-recall-gate.mjs" "$BASELINE_FILE" "$CURRENT_FILE"
