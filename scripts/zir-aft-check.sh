#!/usr/bin/env bash
# shellcheck shell=bash

set -Eeuo pipefail

# /**
#  * AFT Docker check runner.
#  *
#  * Purpose:
#  * Run Rust, TypeScript/Bun, workflow, dependency, coverage, and optional deep
#  * checks for the AFT repository without requiring the host machine to have
#  * Rust, Bun, C/C++ build tooling, actionlint, or Cargo QA tools installed.
#  * The only host dependency is Docker plus Bash.
#  *
#  * Intended users:
#  * - Human developers doing local checks before commit or push.
#  * - AI coding agents that need a deterministic project validation command
#  *   before proposing or committing code changes.
#  *
#  * Agent usage policy:
#  * - After a small edit: run `./scripts/aft-check.sh quick`.
#  * - Before a git commit: run `./scripts/aft-check.sh validate`.
#  * - After editing Cargo.toml/Cargo.lock/dependency policy: run
#  *   `./scripts/aft-check.sh deps` or `./scripts/aft-check.sh security`.
#  * - After risky parser/edit/filesystem/process/concurrency changes: run
#  *   `./scripts/aft-check.sh deep` before finalizing.
#  * - If coverage is slow on the current machine, use
#  *   `./scripts/aft-check.sh validate --no-coverage` during the edit loop and
#  *   `./scripts/aft-check.sh coverage` before commit.
#  *
#  * Cache policy:
#  * - Cargo downloads, installed Cargo QA tools, target artifacts, Bun package
#  *   downloads, Bun home, and node_modules live in Docker named volumes.
#  * - This script records a `.aft-check-last-used` timestamp in each cache volume.
#  * - Docker has no native "delete this volume exactly 1h after last use" TTL.
#  *   Therefore `--prune-after 1h` prunes stale caches at the start of a run,
#  *   and the explicit `prune-caches` task can be scheduled by cron/systemd.
#  *
#  * @typedef {"validate"|"quick"|"rust"|"ts"|"coverage"|"security"|"deps"|"deep"|"fmt"|"autofmt"|"check"|"clippy"|"nextest"|"doctest"|"audit"|"deny"|"shear"|"hack"|"miri"|"mutants"|"fuzz"|"workflows"|"shell"|"cache-info"|"prune-caches"|"clean-caches"|"help"} TaskName
#  *
#  * @typedef {Object} ValidationProfile
#  * @property {boolean} coverage Included by default in `validate` and `rust`.
#  *   Disable with `--no-coverage` when coverage exceeds the desired edit-loop
#  *   budget; run the standalone `coverage` task before commit.
#  * @property {boolean} deep Disabled by default. Enable with `--with-deep` or
#  *   run `deep` manually because mutation testing and Miri can be expensive.
#  * @property {boolean} typescript Included by default in `validate`; disable
#  *   with `--skip-ts` only for Rust-only edits where speed matters.
#  * @property {boolean} failFast Enabled by default. Use `--keep-going` when an
#  *   agent should collect all independent failures in a single report.
#  */

SCRIPT_NAME="$(basename "$0")"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd -W 2>/dev/null || pwd -P)"

TASK="validate"
FAIL_UNDER=80
SKIP_COVERAGE=0
SKIP_TS=0
WITH_DEEP=0
KEEP_GOING=0
NO_PRUNE=0
PRUNE_AFTER="1h"
FUZZ_TARGET=""
FUZZ_ARGS=()
REBUILD_IMAGES=0
INCLUDE_IMAGES_ON_CLEAN=0
CARGO_FEATURES=""

RUST_BASE_IMAGE="${AFT_RUST_BASE_IMAGE:-rust:1-bookworm}"
RUST_CHECK_IMAGE="${AFT_RUST_CHECK_IMAGE:-aft-check-rust:bookworm}"
RUST_NIGHTLY_BASE_IMAGE="${AFT_RUST_NIGHTLY_BASE_IMAGE:-rust:nightly-bookworm}"
RUST_NIGHTLY_CHECK_IMAGE="${AFT_RUST_NIGHTLY_CHECK_IMAGE:-aft-check-rust:nightly-bookworm}"
BUN_IMAGE="${AFT_BUN_IMAGE:-oven/bun:1-debian}"
ACTIONLINT_IMAGE="${AFT_ACTIONLINT_IMAGE:-rhysd/actionlint:latest}"
BUSYBOX_IMAGE="${AFT_BUSYBOX_IMAGE:-busybox:1.36}"

HOST_UID="$(id -u)"
HOST_GID="$(id -g)"
DOCKER_PULL_POLICY="${AFT_DOCKER_PULL_POLICY:-missing}"

CACHE_PREFIX="${AFT_CHECK_CACHE_PREFIX:-aft-check}"
V_CARGO_HOME="${CACHE_PREFIX}-cargo-home"
V_CARGO_TOOLS="${CACHE_PREFIX}-cargo-tools"
V_TARGET="${CACHE_PREFIX}-target"
V_BUN_CACHE="${CACHE_PREFIX}-bun-cache"
V_BUN_HOME="${CACHE_PREFIX}-bun-home"
V_NODE_MODULES="${CACHE_PREFIX}-node-modules"
CACHE_VOLUMES=(
  "$V_CARGO_HOME"
  "$V_CARGO_TOOLS"
  "$V_TARGET"
  "$V_BUN_CACHE"
  "$V_BUN_HOME"
  "$V_NODE_MODULES"
)

FAILURES=()
SUCCESSES=()
STARTED_AT="$(date +%s)"

usage() {
  cat <<EOF
Usage:
  $SCRIPT_NAME [task] [options]

Default task:
  validate

Common tasks:
  validate       Full normal local gate: fmt, check, clippy, nextest, doctest,
                 TypeScript/Bun checks, coverage, security, workflows.
  quick          Faster edit-loop gate: fmt, check, clippy, nextest, TypeScript.
                 No coverage, no dependency/security scan.
  rust           Rust-only normal gate: fmt, check, clippy, nextest, doctest,
                 coverage, security.
  ts             Bun install, typecheck, lint, and tests inside Docker.
  coverage       cargo-llvm-cov + nextest coverage gate.
  security       cargo audit + cargo deny if deny.toml exists.
  deps           security + cargo shear dependency hygiene.
  deep           Expensive optional checks: cargo-hack feature matrix, targeted
                 Miri, and cargo-mutants. Use before release or risky refactors.

Individual Rust tasks:
  fmt, autofmt, check, clippy, nextest, doctest, audit, deny, shear, hack,
  miri, mutants, fuzz

Other tasks:
  workflows      Lint GitHub Actions workflows with actionlint in Docker.
  shell          Open an interactive shell in the Rust check container.
  cache-info     Show Docker cache volume metadata.
  prune-caches   Remove stale cache volumes older than --prune-after.
  clean-caches   Remove all check cache volumes. Add --include-images to also
                 remove locally built helper images.
  help           Show this help.

Options:
  --fail-under N       Coverage line threshold. Default: 80.
  --no-coverage        Skip coverage in validate/rust.
  --skip-ts            Skip TypeScript/Bun checks in validate.
  --with-deep          Append deep checks to validate/rust.
  --keep-going         Continue after failures and summarize all failures.
  --fail-fast          Stop after first failure. Default behavior.
  --prune-after TTL    Stale cache TTL. Examples: 1h, 45m, 3600s. Default: 1h.
  --no-prune           Do not prune stale caches before running checks.
  --fuzz-target NAME   Required for task fuzz unless AFT_FUZZ_TARGET is set.
  --features FEATURES  Cargo features to enable (e.g. semantic-model2vec).
                        Applied to check, clippy, nextest, doctest, coverage.
  --rebuild-images     Rebuild local Rust helper images before running.
  --include-images     With clean-caches, also remove helper images.
  -h, --help           Show this help.

Environment overrides:
  AFT_RUST_BASE_IMAGE             Default: rust:1-bookworm
  AFT_RUST_CHECK_IMAGE            Default: aft-check-rust:bookworm
  AFT_RUST_NIGHTLY_BASE_IMAGE     Default: rust:nightly-bookworm
  AFT_RUST_NIGHTLY_CHECK_IMAGE    Default: aft-check-rust:nightly-bookworm
  AFT_BUN_IMAGE                   Default: oven/bun:1-debian
  AFT_ACTIONLINT_IMAGE            Default: rhysd/actionlint:latest
  AFT_CHECK_CACHE_PREFIX          Default: aft-check
  AFT_DOCKER_PULL_POLICY          Default: missing

Examples:
  ./scripts/aft-check.sh quick
  ./scripts/aft-check.sh validate
  ./scripts/aft-check.sh validate --no-coverage
  ./scripts/aft-check.sh rust --with-deep
  ./scripts/aft-check.sh coverage --fail-under 75
  ./scripts/aft-check.sh deps
  ./scripts/aft-check.sh deep
  ./scripts/aft-check.sh check --features semantic-model2vec
  ./scripts/aft-check.sh quick --features semantic-model2vec
  ./scripts/aft-check.sh fuzz --fuzz-target parser_payload -- -runs=100000
  ./scripts/aft-check.sh prune-caches --prune-after 1h
EOF
}

log() { printf '%s\n' "$*"; }
warn() { printf 'WARN: %s\n' "$*" >&2; }
fatal() { printf 'ERROR: %s\n' "$*" >&2; exit 2; }

have() { command -v "$1" >/dev/null 2>&1; }

require_docker() {
  have docker || fatal "Docker is required but was not found on PATH."
  docker info >/dev/null 2>&1 || fatal "Docker is installed but the Docker daemon is not reachable."
}

parse_ttl_seconds() {
  local ttl="$1"
  case "$ttl" in
    *s) printf '%s\n' "${ttl%s}" ;;
    *m) printf '%s\n' "$(( ${ttl%m} * 60 ))" ;;
    *h) printf '%s\n' "$(( ${ttl%h} * 3600 ))" ;;
    *d) printf '%s\n' "$(( ${ttl%d} * 86400 ))" ;;
    ''|*[!0-9]*) fatal "Invalid TTL '$ttl'. Use examples like 3600s, 45m, 1h." ;;
    *) printf '%s\n' "$ttl" ;;
  esac
}

ensure_volume() {
  local volume="$1"
  if ! docker volume inspect "$volume" >/dev/null 2>&1; then
    docker volume create \
      --label aft.check.cache=true \
      --label aft.check.cache.prefix="$CACHE_PREFIX" \
      "$volume" >/dev/null
  fi
}

volume_exists() {
  docker volume inspect "$1" >/dev/null 2>&1
}

touch_volume() {
  local volume="$1"
  ensure_volume "$volume"
  docker run --rm \
    -v "$volume:/cache" \
    "$BUSYBOX_IMAGE" \
    sh -c "chown -R '$HOST_UID:$HOST_GID' /cache 2>/dev/null || true; date +%s > /cache/.aft-check-last-used" >/dev/null
}

read_volume_last_used() {
  local volume="$1"
  if ! volume_exists "$volume"; then
    printf '0\n'
    return
  fi
  docker run --rm \
    -v "$volume:/cache:ro" \
    "$BUSYBOX_IMAGE" \
    sh -c 'cat /cache/.aft-check-last-used 2>/dev/null || echo 0' 2>/dev/null || printf '0\n'
}

init_cache_volumes() {
  local volume
  for volume in "${CACHE_VOLUMES[@]}"; do
    touch_volume "$volume"
  done
}

mark_caches_used() {
  local volume
  for volume in "${CACHE_VOLUMES[@]}"; do
    if volume_exists "$volume"; then
      touch_volume "$volume"
    fi
  done
}

prune_stale_caches() {
  require_docker
  local ttl_seconds now volume last age
  ttl_seconds="$(parse_ttl_seconds "$PRUNE_AFTER")"
  now="$(date +%s)"

  log "Pruning cache volumes unused for >= ${PRUNE_AFTER} (${ttl_seconds}s)."
  for volume in "${CACHE_VOLUMES[@]}"; do
    if ! volume_exists "$volume"; then
      continue
    fi
    last="$(read_volume_last_used "$volume")"
    if [[ ! "$last" =~ ^[0-9]+$ ]] || [[ "$last" == "0" ]]; then
      warn "Volume $volume has no valid last-used marker; keeping it."
      continue
    fi
    age=$(( now - last ))
    if (( age >= ttl_seconds )); then
      log "Removing stale volume $volume (idle ${age}s)."
      docker volume rm "$volume" >/dev/null || warn "Could not remove $volume; it may be in use."
    fi
  done
}

cache_info() {
  require_docker
  local now volume last age size_line
  now="$(date +%s)"
  printf '%-34s %-14s %-12s %s\n' "VOLUME" "LAST_USED" "IDLE_SECONDS" "SIZE"
  for volume in "${CACHE_VOLUMES[@]}"; do
    if ! volume_exists "$volume"; then
      printf '%-34s %-14s %-12s %s\n' "$volume" "missing" "-" "-"
      continue
    fi
    last="$(read_volume_last_used "$volume")"
    if [[ "$last" =~ ^[0-9]+$ ]] && (( last > 0 )); then
      age=$(( now - last ))
    else
      age="unknown"
    fi
    size_line="$(docker run --rm -v "$volume:/cache:ro" "$BUSYBOX_IMAGE" sh -c 'du -sh /cache 2>/dev/null | cut -f1' 2>/dev/null || true)"
    printf '%-34s %-14s %-12s %s\n' "$volume" "$last" "$age" "${size_line:-unknown}"
  done
}

clean_caches() {
  require_docker
  local volume
  for volume in "${CACHE_VOLUMES[@]}"; do
    if volume_exists "$volume"; then
      log "Removing volume $volume"
      docker volume rm "$volume" >/dev/null || warn "Could not remove $volume; it may be in use."
    fi
  done

  if (( INCLUDE_IMAGES_ON_CLEAN )); then
    for image in "$RUST_CHECK_IMAGE" "$RUST_NIGHTLY_CHECK_IMAGE"; do
      if docker image inspect "$image" >/dev/null 2>&1; then
        log "Removing image $image"
        docker image rm "$image" >/dev/null || warn "Could not remove image $image."
      fi
    done
  fi
}

quote_cmd() {
  local out=() arg
  for arg in "$@"; do
    out+=("$(printf '%q' "$arg")")
  done
  printf '%s ' "${out[@]}"
}

ensure_rust_image() {
  require_docker
  if (( REBUILD_IMAGES )) || ! docker image inspect "$RUST_CHECK_IMAGE" >/dev/null 2>&1; then
    log "Building local Rust check image: $RUST_CHECK_IMAGE from $RUST_BASE_IMAGE"
    docker build --pull=false \
      --label aft.check.image=true \
      --build-arg RUST_BASE_IMAGE="$RUST_BASE_IMAGE" \
      -t "$RUST_CHECK_IMAGE" \
      -f - . <<'DOCKERFILE'
ARG RUST_BASE_IMAGE=rust:1-bookworm
FROM ${RUST_BASE_IMAGE}
RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    ca-certificates clang cmake curl git libssl-dev make perl pkg-config unzip xz-utils \
  && rm -rf /var/lib/apt/lists/*
RUN rustup component add rustfmt clippy
ENV CARGO_INCREMENTAL=0 RUST_BACKTRACE=1
DOCKERFILE
  fi
}

ensure_rust_nightly_image() {
  require_docker
  if (( REBUILD_IMAGES )) || ! docker image inspect "$RUST_NIGHTLY_CHECK_IMAGE" >/dev/null 2>&1; then
    log "Building local Rust nightly check image: $RUST_NIGHTLY_CHECK_IMAGE from $RUST_NIGHTLY_BASE_IMAGE"
    docker build --pull=false \
      --label aft.check.image=true \
      --build-arg RUST_BASE_IMAGE="$RUST_NIGHTLY_BASE_IMAGE" \
      -t "$RUST_NIGHTLY_CHECK_IMAGE" \
      -f - . <<'DOCKERFILE'
ARG RUST_BASE_IMAGE=rust:nightly-bookworm
FROM ${RUST_BASE_IMAGE}
RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    ca-certificates clang cmake curl git libssl-dev make perl pkg-config unzip xz-utils \
  && rm -rf /var/lib/apt/lists/*
RUN rustup component add rustfmt clippy miri
ENV CARGO_INCREMENTAL=0 RUST_BACKTRACE=1
DOCKERFILE
  fi
}

rust_docker_args() {
  local image="$1"
  printf '%s\0' \
    run --rm "--pull=$DOCKER_PULL_POLICY" \
    --workdir /work \
    --user "$HOST_UID:$HOST_GID" \
    --mount "type=bind,source=$REPO_ROOT,target=/work" \
    --volume "$V_CARGO_HOME:/cargo-home" \
    --volume "$V_CARGO_TOOLS:/cargo-tools" \
    --volume "$V_TARGET:/target" \
    --env CARGO_HOME=/cargo-home \
    --env CARGO_INSTALL_ROOT=/cargo-tools \
    --env CARGO_TARGET_DIR=/target \
    --env CARGO_INCREMENTAL=0 \
    --env RUST_BACKTRACE=1 \
    --env HOME=/tmp \
    "$image" bash -lc
}

bun_docker_args() {
  printf '%s\0' \
    run --rm "--pull=$DOCKER_PULL_POLICY" \
    --workdir /work \
    --user "$HOST_UID:$HOST_GID" \
    --mount "type=bind,source=$REPO_ROOT,target=/work" \
    --volume "$V_BUN_CACHE:/bun-cache" \
    --volume "$V_BUN_HOME:/bun-home" \
    --volume "$V_NODE_MODULES:/work/node_modules" \
    --env HOME=/bun-home \
    --env BUN_INSTALL_CACHE_DIR=/bun-cache \
    "$BUN_IMAGE" bash -lc
}

run_step() {
  local label="$1"
  shift

  log ""
  log "=== $label ==="
  log "+ $(quote_cmd "$@")"

  local code=0
  set +e
  "$@"
  code=$?
  set -e

  if (( code == 0 )); then
    log "OK: $label"
    SUCCESSES+=("$label")
  else
    log "FAILED: $label (exit $code)"
    FAILURES+=("$label:$code")
    if (( ! KEEP_GOING )); then
      summarize_and_exit 1
    fi
  fi
}

run_rust() {
  local label="$1"
  local command="$2"
  ensure_rust_image
  local args=()
  while IFS= read -r -d '' part; do args+=("$part"); done < <(rust_docker_args "$RUST_CHECK_IMAGE")
  run_step "$label" docker "${args[@]}" "set -Eeuo pipefail; export PATH=/cargo-tools/bin:/usr/local/cargo/bin:\$PATH; $command"
}

run_rust_nightly() {
  local label="$1"
  local command="$2"
  ensure_rust_nightly_image
  local args=()
  while IFS= read -r -d '' part; do args+=("$part"); done < <(rust_docker_args "$RUST_NIGHTLY_CHECK_IMAGE")
  run_step "$label" docker "${args[@]}" "set -Eeuo pipefail; export PATH=/cargo-tools/bin:/usr/local/cargo/bin:\$PATH; $command"
}

run_bun() {
  local label="$1"
  local command="$2"
  local args=()
  while IFS= read -r -d '' part; do args+=("$part"); done < <(bun_docker_args)
  run_step "$label" docker "${args[@]}" "set -Eeuo pipefail; $command"
}

install_cargo_tool_cmd() {
  local binary="$1"
  local crate="$2"
  printf 'if ! command -v %q >/dev/null 2>&1; then cargo install %q --locked; fi' "$binary" "$crate"
}

cargo_features_flag() {
  if [[ -n "$CARGO_FEATURES" ]]; then
    printf '%s' "--features $CARGO_FEATURES"
  fi
}

bun_install_cmd() {
  cat <<'EOF'
if [ -f bun.lock ] || [ -f bun.lockb ]; then
  bun install --frozen-lockfile
else
  bun install
fi
EOF
}

task_fmt() { run_rust "fmt" "cargo fmt --all -- --check"; }
task_autofmt() { run_rust "autofmt" "cargo fmt --all"; }
task_check() { run_rust "check" "cargo check --workspace --all-targets --locked $(cargo_features_flag)"; }
task_clippy() { run_rust "clippy" "cargo clippy --workspace --all-targets --locked $(cargo_features_flag) -- -D warnings"; }
task_nextest() {
  local install_nextest
  install_nextest="$(install_cargo_tool_cmd cargo-nextest cargo-nextest)"
  run_rust "nextest" "$install_nextest; cargo nextest run --workspace --locked $(cargo_features_flag)"
}
task_doctest() { run_rust "doctest" "cargo test --doc --workspace --locked $(cargo_features_flag)"; }
task_coverage() {
  local install_nextest install_cov
  install_nextest="$(install_cargo_tool_cmd cargo-nextest cargo-nextest)"
  install_cov="$(install_cargo_tool_cmd cargo-llvm-cov cargo-llvm-cov)"
  run_rust "coverage" "$install_nextest; $install_cov; mkdir -p target/coverage; cargo llvm-cov nextest --workspace --locked $(cargo_features_flag) --lcov --output-path target/coverage/lcov.info --fail-under-lines $FAIL_UNDER"
}
task_audit() {
  local install_audit
  install_audit="$(install_cargo_tool_cmd cargo-audit cargo-audit)"
  run_rust "audit" "$install_audit; cargo audit"
}
task_deny() {
  local install_deny
  install_deny="$(install_cargo_tool_cmd cargo-deny cargo-deny)"
  run_rust "deny" "if [ -f deny.toml ] || [ -f .cargo/deny.toml ]; then $install_deny; cargo deny check; else echo 'SKIP: no deny.toml or .cargo/deny.toml found.'; fi"
}
task_shear() {
  local install_shear
  install_shear="$(install_cargo_tool_cmd cargo-shear cargo-shear)"
  run_rust "shear" "$install_shear; cargo shear --deny-warnings"
}
task_hack() {
  local install_hack
  install_hack="$(install_cargo_tool_cmd cargo-hack cargo-hack)"
  run_rust "feature-matrix" "$install_hack; cargo hack check --workspace --locked --each-feature --no-dev-deps"
}
task_miri() {
  # Keep Miri targeted. The main aft crate is OS/process/PTY/FFI-heavy; broad
  # Miri runs are likely noisy. Expand this when pure modules become compatible.
  run_rust_nightly "miri-aft-tokenizer" "cargo miri test -p aft-tokenizer"
}
task_mutants() {
  local install_mutants
  install_mutants="$(install_cargo_tool_cmd cargo-mutants cargo-mutants)"
  run_rust "mutants" "$install_mutants; cargo mutants --workspace"
}
task_fuzz() {
  local target="${FUZZ_TARGET:-${AFT_FUZZ_TARGET:-}}"
  [[ -n "$target" ]] || fatal "fuzz requires --fuzz-target NAME or AFT_FUZZ_TARGET=NAME."
  local install_fuzz fuzz_extra
  install_fuzz="$(install_cargo_tool_cmd cargo-fuzz cargo-fuzz)"
  fuzz_extra="${FUZZ_ARGS[*]:-}"
  run_rust_nightly "fuzz:$target" "$install_fuzz; cargo fuzz run '$target' $fuzz_extra"
}
task_ts() {
  local install
  install="$(bun_install_cmd)"
  run_bun "typescript-and-bun" "$install; bun run typecheck; bun run lint; bun run --filter '*' test"
}
task_workflows() {
  # Run through a shell inside the image so .github/workflows/*.yml expands
  # inside the mounted repository, not on the host running this script.
  run_step "workflow-lint" docker run --rm "--pull=$DOCKER_PULL_POLICY" \
    --workdir /work \
    --mount "type=bind,source=$REPO_ROOT,target=/work" \
    --entrypoint sh \
    "$ACTIONLINT_IMAGE" -lc 'actionlint -color .github/workflows/*.yml'
}
task_security() {
  task_audit
  task_deny
}
task_deps() {
  task_security
  task_shear
}
task_deep() {
  task_hack
  task_miri
  task_mutants
}
task_quick() {
  task_fmt
  task_check
  task_clippy
  task_nextest
  if (( ! SKIP_TS )); then task_ts; fi
}
task_rust() {
  task_fmt
  task_check
  task_clippy
  task_nextest
  task_doctest
  if (( ! SKIP_COVERAGE )); then task_coverage; fi
  task_security
  if (( WITH_DEEP )); then task_deep; fi
}
task_validate() {
  task_fmt
  task_check
  task_clippy
  task_nextest
  task_doctest
  if (( ! SKIP_TS )); then task_ts; fi
  if (( ! SKIP_COVERAGE )); then task_coverage; fi
  task_security
  task_workflows
  if (( WITH_DEEP )); then task_deep; fi
}
task_shell() {
  ensure_rust_image
  local args=()
  while IFS= read -r -d '' part; do args+=("$part"); done < <(rust_docker_args "$RUST_CHECK_IMAGE")
  log "+ docker ${args[*]} bash"
  exec docker "${args[@]}" "export PATH=/cargo-tools/bin:/usr/local/cargo/bin:\$PATH; exec bash"
}

summarize_and_exit() {
  local code="${1:-0}"
  local elapsed=$(( $(date +%s) - STARTED_AT ))
  mark_caches_used || true
  log ""
  log "──────────────────────────────────────────────────"
  log "AFT check summary (${elapsed}s)"
  if ((${#SUCCESSES[@]})); then
    log "Passed:"
    printf '  - %s\n' "${SUCCESSES[@]}"
  fi
  if ((${#FAILURES[@]})); then
    log "Failed:"
    printf '  - %s\n' "${FAILURES[@]}"
    exit 1
  fi
  log "All selected checks passed."
  exit "$code"
}

parse_args() {
  if (($# > 0)); then
    case "$1" in
      -h|--help) TASK="help"; shift ;;
      --*) ;;
      *) TASK="$1"; shift ;;
    esac
  fi

  while (($# > 0)); do
    case "$1" in
      --fail-under)
        shift; [[ $# -gt 0 ]] || fatal "--fail-under requires a value"; FAIL_UNDER="$1" ;;
      --fail-under=*) FAIL_UNDER="${1#*=}" ;;
      --no-coverage) SKIP_COVERAGE=1 ;;
      --skip-ts) SKIP_TS=1 ;;
      --with-deep) WITH_DEEP=1 ;;
      --keep-going) KEEP_GOING=1 ;;
      --fail-fast) KEEP_GOING=0 ;;
      --prune-after)
        shift; [[ $# -gt 0 ]] || fatal "--prune-after requires a value"; PRUNE_AFTER="$1" ;;
      --prune-after=*) PRUNE_AFTER="${1#*=}" ;;
      --no-prune) NO_PRUNE=1 ;;
      --fuzz-target)
        shift; [[ $# -gt 0 ]] || fatal "--fuzz-target requires a value"; FUZZ_TARGET="$1" ;;
      --fuzz-target=*) FUZZ_TARGET="${1#*=}" ;;
      --features)
        shift; [[ $# -gt 0 ]] || fatal "--features requires a value"; CARGO_FEATURES="$1" ;;
      --features=*) CARGO_FEATURES="${1#*=}" ;;
      --rebuild-images) REBUILD_IMAGES=1 ;;
      --include-images) INCLUDE_IMAGES_ON_CLEAN=1 ;;
      --)
        shift; FUZZ_ARGS+=("$@"); break ;;
      -h|--help) TASK="help" ;;
      *) fatal "Unknown option or argument: $1" ;;
    esac
    shift || true
  done

  [[ "$FAIL_UNDER" =~ ^[0-9]+$ ]] || fatal "--fail-under must be an integer from 0 to 100."
  (( FAIL_UNDER >= 0 && FAIL_UNDER <= 100 )) || fatal "--fail-under must be from 0 to 100."
}

main() {
  parse_args "$@"

  case "$TASK" in
    help) usage; exit 0 ;;
  esac

  require_docker

  case "$TASK" in
    clean-caches) clean_caches; exit 0 ;;
    cache-info) cache_info; exit 0 ;;
    prune-caches) prune_stale_caches; exit 0 ;;
  esac

  if (( ! NO_PRUNE )); then
    prune_stale_caches
  fi
  init_cache_volumes

  # Ensure CRLF line endings don't cause test failures inside Linux Docker containers.
  # Golden/fixture files with \r\n produce byte-comparison failures when the Rust
  # process (running inside Linux Docker) outputs LF. Setting this once here is safe
  # because the repo is bind-mounted into Docker.
  if [[ "$(git -C "$REPO_ROOT" config core.autocrlf 2>/dev/null)" != "false" ]]; then
    log "Setting core.autocrlf=false for Docker test compatibility"
    git -C "$REPO_ROOT" config core.autocrlf false
  fi

  case "$TASK" in
    validate) task_validate ;;
    quick) task_quick ;;
    rust) task_rust ;;
    ts) task_ts ;;
    coverage|cov) task_coverage ;;
    security) task_security ;;
    deps) task_deps ;;
    deep) task_deep ;;
    fmt) task_fmt ;;
    autofmt) task_autofmt ;;
    check) task_check ;;
    clippy) task_clippy ;;
    nextest) task_nextest ;;
    doctest) task_doctest ;;
    audit) task_audit ;;
    deny) task_deny ;;
    shear) task_shear ;;
    hack) task_hack ;;
    miri) task_miri ;;
    mutants) task_mutants ;;
    fuzz) task_fuzz ;;
    workflows) task_workflows ;;
    shell) task_shell ;;
    *) fatal "Unknown task '$TASK'. Run '$SCRIPT_NAME help'." ;;
  esac

  summarize_and_exit 0
}

main "$@"
