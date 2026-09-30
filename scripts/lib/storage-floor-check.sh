#!/usr/bin/env bash
# Placement gate: refuse an AFT build that cannot read what the live storage
# root holds.
#
# Every AFT build that supports rollback safety keeps a monotonic reader floor
# at <storage root>/reader-floor.json: for each on-disk store, the lowest
# format version a reader must understand. A candidate build reports the
# highest format version it reads for each store through `aft --formats`, a
# side-effect-free flag (no storage, no daemon, no logging). The candidate is
# refused when, for any store in the floor, it reports a lower version or does
# not know the store at all, or when it cannot report its formats (a build
# older than the floor scheme), or when the floor file itself cannot be
# interpreted. A root with no floor file yet has never been opened by a
# floor-aware build, so only formats every build reads are on disk; the check
# passes and says so.
#
# Sourced by scripts/stage-card.sh (function ck_aft_check_storage_floor), and
# runnable directly to vet any image, a rollback image included:
#   scripts/lib/storage-floor-check.sh <candidate-binary> [storage-root]
#
# The storage root defaults to the live one, resolved like the product does
# for a plugin-less launch: CK_AFT_STORAGE_DIR, then AFT_STORAGE_DIR, then
# $AFT_CACHE_DIR/aft, then $XDG_DATA_HOME/cortexkit/aft, then
# ~/.local/share/cortexkit/aft. A deployment that configures a different
# storage_dir must pass it explicitly.
#
# Staging is advisory for a remote destination: the placer must repeat this
# check against the destination root immediately before replacing the binary.

ck_aft_default_storage_root() {
  if [ -n "${CK_AFT_STORAGE_DIR:-}" ]; then
    printf '%s\n' "$CK_AFT_STORAGE_DIR"
  elif [ -n "${AFT_STORAGE_DIR:-}" ]; then
    printf '%s\n' "$AFT_STORAGE_DIR"
  elif [ -n "${AFT_CACHE_DIR:-}" ]; then
    printf '%s/aft\n' "$AFT_CACHE_DIR"
  elif [ -n "${XDG_DATA_HOME:-}" ]; then
    printf '%s/cortexkit/aft\n' "$XDG_DATA_HOME"
  else
    printf '%s/.local/share/cortexkit/aft\n' "$HOME"
  fi
}

# ck_aft_check_storage_floor <candidate-binary> <storage-root>
# Returns 0 when the candidate can read everything the floor requires, 1 (with
# the blocking store named on stderr) otherwise.
ck_aft_check_storage_floor() {
  local candidate="$1" storage_root="$2"
  local floor="$storage_root/reader-floor.json"
  local formats
  if ! formats="$("$candidate" --formats 2>/dev/null)" || [ -z "$formats" ]; then
    echo "storage-floor: $candidate does not report its on-disk formats (aft --formats); refusing a build that cannot be checked against $floor" >&2
    return 1
  fi
  if [ ! -e "$floor" ]; then
    echo "storage-floor: no reader floor at $floor yet (no floor-aware build has opened this root); nothing to refuse"
    return 0
  fi
  CK_FLOOR_PATH="$floor" CK_CANDIDATE="$candidate" CK_FORMATS="$formats" python3 - <<'PY'
import json, os, sys

floor_path = os.environ["CK_FLOOR_PATH"]
candidate = os.environ["CK_CANDIDATE"]

def refuse(message):
    print(f"storage-floor: refusing {candidate}: {message}", file=sys.stderr)
    sys.exit(1)

try:
    formats = json.loads(os.environ["CK_FORMATS"])
    supported = formats["stores"]
    if not isinstance(supported, dict):
        raise ValueError("stores is not an object")
except Exception as error:  # noqa: BLE001 - any malformed report is a refusal
    refuse(f"unreadable --formats output ({error})")

try:
    with open(floor_path, "rb") as handle:
        floor = json.load(handle)
    floor_schema = floor["floor_schema"]
    required = floor["stores"]
    if not isinstance(required, dict):
        raise ValueError("stores is not an object")
except Exception as error:  # noqa: BLE001 - an unreadable floor proves nothing safe
    refuse(f"reader floor {floor_path} cannot be read ({error})")

candidate_floor_schema = formats.get("floor_schema")
if not isinstance(floor_schema, int) or not isinstance(candidate_floor_schema, int) \
        or floor_schema > candidate_floor_schema:
    refuse(
        f"reader floor {floor_path} has floor_schema {floor_schema!r}; "
        f"the candidate reads floor_schema {candidate_floor_schema!r}"
    )

blocking = []
for store, needed in sorted(required.items()):
    have = supported.get(store)
    if not isinstance(needed, int) or not isinstance(have, int) or have < needed:
        blocking.append(f"{store} needs format {needed}, candidate reads {have if have is not None else 'none'}")
if blocking:
    refuse(f"storage root {os.path.dirname(floor_path)} is below its reader floor for: " + "; ".join(blocking))

print(f"storage-floor: {candidate} reads every store in {floor_path}")
PY
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  set -euo pipefail
  if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
    echo "usage: $0 <candidate-binary> [storage-root]" >&2
    exit 2
  fi
  ck_aft_check_storage_floor "$1" "${2:-$(ck_aft_default_storage_root)}"
fi
