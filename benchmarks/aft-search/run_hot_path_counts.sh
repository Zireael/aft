#!/usr/bin/env bash
# Run only the opt-in counters, serially, so process-wide allocations are attributable.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"
python3 benchmarks/aft-search/run_hot_path_counts.py --corpus "${1:-$ROOT}" "${@:2}"
