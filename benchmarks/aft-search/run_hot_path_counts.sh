#!/usr/bin/env bash
# Run only the opt-in counters, serially, so process-wide allocations are attributable.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"
export CARGO_BUILD_RUSTC_WRAPPER= RUSTC_WRAPPER=
export AFT_PERF_CORPUS=${1:-$ROOT}
cargo test -p agent-file-tools --lib hot_path -- --ignored --nocapture --test-threads=1
