#!/usr/bin/env bash
# Native x64 Windows pre-push smoke; only committed Git objects cross the wire.
set -euo pipefail
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "$script_dir/lib/windows-gate.py" "$@"
