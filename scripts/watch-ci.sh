#!/usr/bin/env bash
# Watch a GitHub Actions workflow run for a specific commit, fail-fast on any
# job failure. Complements wait-release.sh (which watches the release workflow
# by tag); this one watches an arbitrary workflow (default "Tests") for the
# push run of a given commit and reports the instant ANY job concludes failure.
#
# Usage:
#   ./scripts/watch-ci.sh [<commit-ish>] [<workflow-name>]
#   ./scripts/watch-ci.sh                 # HEAD, "Tests" workflow
#   ./scripts/watch-ci.sh fc24198a        # that commit, "Tests" workflow
#   ./scripts/watch-ci.sh HEAD Release    # HEAD, "Release" workflow
#
# Exit codes: 0 = all jobs succeeded, 1 = a job failed (or run non-success),
#             2 = timed out waiting.
#
# Why fail-fast: the full Tests matrix (unit + e2e across 4 platforms + pi-rpc)
# runs ~15-20 min. When one job fails at minute 3 there is no reason to wait out
# the rest — this prints the failing job name and returns immediately so you can
# pull the log and fix instead of watching the green jobs finish.
#
# Output uses plain newlines so line-buffered pipes flush each status update.

set -euo pipefail

COMMITISH="${1:-HEAD}"
WORKFLOW="${2:-Tests}"
REPO="${AFT_CI_REPO:-cortexkit/aft}"
MAX_WAIT="${MAX_WAIT_SECONDS:-2400}"
INTERVAL="${WATCH_CI_INTERVAL:-30}"

if ! command -v gh >/dev/null 2>&1; then
  echo "error: gh CLI not found on PATH" >&2
  exit 2
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq not found on PATH" >&2
  exit 2
fi

FULL_SHA="$(git rev-parse "$COMMITISH")"
SHORT_SHA="$(git rev-parse --short "$COMMITISH")"
echo "watching '$WORKFLOW' CI for $SHORT_SHA ($FULL_SHA) in $REPO"

DEADLINE=$(( $(date +%s) + MAX_WAIT ))
# Newline-delimited "job\tstatus/conc" lines already printed (bash 3.2-safe; no
# associative arrays — macOS ships bash 3.2).
SEEN_JOB_STATE=""

while :; do
  if [ "$(date +%s)" -ge "$DEADLINE" ]; then
    echo "RESULT=TIMEOUT (waited ${MAX_WAIT}s)"
    exit 2
  fi

  RUN_ID="$(gh run list -R "$REPO" --workflow "$WORKFLOW" --limit 15 \
    --json databaseId,headSha \
    --jq ".[] | select(.headSha==\"$FULL_SHA\") | .databaseId" 2>/dev/null | head -1)"
  if [ -z "$RUN_ID" ]; then
    echo "· no '$WORKFLOW' run for $SHORT_SHA yet; waiting…"
    sleep "$INTERVAL"
    continue
  fi

  RUN_JSON="$(gh run view "$RUN_ID" -R "$REPO" --json status,conclusion,jobs 2>/dev/null)"
  RUN_STATUS="$(echo "$RUN_JSON" | jq -r '.status')"

  # Print per-job transitions once (membership via grep -F, bash 3.2-safe).
  while IFS=$'\t' read -r jname jstatus jconc; do
    line="${jname}=${jstatus}/${jconc}"
    if ! printf '%s' "$SEEN_JOB_STATE" | grep -qF -- "$line"; then
      SEEN_JOB_STATE="${SEEN_JOB_STATE}${line}
"
      echo "  · ${jstatus}/${jconc:-} — ${jname}"
    fi
  done < <(echo "$RUN_JSON" | jq -r '.jobs[] | "\(.name)\t\(.status)\t\(.conclusion // "")"')

  # Fail-fast: any job concluded failure-class.
  FAILED="$(echo "$RUN_JSON" | jq -r '.jobs[] | select(.conclusion=="failure" or .conclusion=="cancelled" or .conclusion=="timed_out") | .name')"
  if [ -n "$FAILED" ]; then
    echo "RESULT=FAILURE (run $RUN_ID)"
    echo "$FAILED" | sed 's/^/  ✗ /'
    echo "https://github.com/$REPO/actions/runs/$RUN_ID"
    exit 1
  fi

  if [ "$RUN_STATUS" = "completed" ]; then
    CONC="$(echo "$RUN_JSON" | jq -r '.conclusion')"
    if [ "$CONC" = "success" ]; then
      echo "RESULT=ALL_GREEN (run $RUN_ID)"
      exit 0
    fi
    echo "RESULT=$CONC (run $RUN_ID)"
    exit 1
  fi

  DONE="$(echo "$RUN_JSON" | jq -r '[.jobs[] | select(.status=="completed")] | length')"
  TOTAL="$(echo "$RUN_JSON" | jq -r '.jobs | length')"
  echo "[$RUN_STATUS] run=$RUN_ID ${DONE}/${TOTAL} jobs done"
  sleep "$INTERVAL"
done
