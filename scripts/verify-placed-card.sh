#!/usr/bin/env bash
# Check the ck-aft binary actually placed at the deploy path, after placement.
#
# Placement copies the staged card and never re-signs it (see
# scripts/stage-card.sh). This reads the placed file itself, not the card:
# a placement that re-signed, or copied the wrong file, shows up here.
#   - macOS: identifier ck-aft, hardened runtime, no get-task-allow, and only
#     the disable-library-validation exception (same rule as staging);
#   - the placed bytes equal the card stage-card.sh last staged, as recorded
#     (hash and name) in $CK_STAGING_DIR/ck-aft.current, when that file exists.
#
# Usage: scripts/verify-placed-card.sh [deploy-path]
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=lib/ck-aft-signature.sh
source "$REPO_ROOT/scripts/lib/ck-aft-signature.sh"

STAGING="${CK_STAGING_DIR:-$HOME/.local/share/cortexkit/staging}"
DEPLOY="${1:-${CK_DEPLOY_PATH:-$HOME/.local/share/cortexkit/bin/ck-aft}}"

if [ ! -f "$DEPLOY" ]; then
  echo "verify-placed-card: nothing placed at $DEPLOY" >&2
  exit 2
fi
echo "==> placed binary: $DEPLOY"
if [ "$(uname -s)" = "Darwin" ]; then
  ck_aft_check_signature "$DEPLOY" verify-placed-card || exit 2
fi

PLACED_HASH="$(shasum -a 256 "$DEPLOY" | awk '{print $1}')"
echo "    sha256: $PLACED_HASH"
if [ -f "$STAGING/ck-aft.current" ]; then
  read -r DECLARED_HASH DECLARED_CARD _ < "$STAGING/ck-aft.current"
  if [ "$PLACED_HASH" != "$DECLARED_HASH" ]; then
    echo "verify-placed-card: placed bytes differ from the declared card $DECLARED_CARD ($DECLARED_HASH); was it re-signed or copied from elsewhere?" >&2
    exit 2
  fi
  echo "    matches declared card $DECLARED_CARD"
else
  echo "    (no $STAGING/ck-aft.current declaration; byte match not checked)"
fi
