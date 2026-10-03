#!/usr/bin/env bash
# Align the governed-docs manifest chain in the exact order the release gate
# verifies it: regenerate surface manifests, commit the alignment, sync the
# release manifest's source_commit to the gate's expectation, restage
# activation evidence, and verify the candidate gate - looping because each
# alignment commit can itself shift the expected source commit once.
#
# Usage: scripts/align-governed-docs.sh   (run from the repo root, clean tree
# apart from governed docs churn; commits whatever alignment it produces)
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

bun scripts/audit-v049-agent-surface.ts --write-allowlist --write-prefix-capture --write-manifest >/dev/null
# Stage only files git already tracks: an untracked draft someone left in
# docs/ must never be committed and pushed by this alignment step.
git add -u docs/
if ! git diff --cached --quiet; then
  git commit -qm "Align governed publication manifests"
fi

for attempt in 1 2 3; do
  set +e
  out=$(node scripts/release-gate-v049.mjs --candidate --evidence docs/v0.49-release-evidence.json 2>&1)
  rc=$?
  set -e
  if [ $rc -eq 0 ]; then
    echo "align-governed-docs: gate green at $(git rev-parse --short HEAD)"
    exit 0
  fi
  expected=$(printf '%s\n' "$out" | sed -n 's/.*expected \([0-9a-f]\{40\}\).*/\1/p' | head -1)
  if [ -z "$expected" ]; then
    echo "align-governed-docs: gate failed for a non-source-commit reason:" >&2
    printf '%s\n' "$out" | tail -5 >&2
    exit 1
  fi
  echo "align-governed-docs: attempt $attempt - aligning release manifest to $expected"
  python3 - "$expected" <<'EOF'
import json, pathlib, sys
p = pathlib.Path("docs/v0.49-release-manifest.json")
d = json.loads(p.read_text())
d["source_commit"] = sys.argv[1]
p.write_text(json.dumps(d, indent=2) + "\n")
EOF
  node scripts/release-gate-v049.mjs --stage --evidence-output docs/v0.49-release-evidence.json >/dev/null
  git add -u docs/
  git diff --cached --quiet || git commit -qm "Align release manifest and evidence"
done

echo "align-governed-docs: still failing after 3 alignment attempts" >&2
exit 1
