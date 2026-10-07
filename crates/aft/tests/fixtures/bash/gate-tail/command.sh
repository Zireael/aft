#!/usr/bin/env bash
set -euo pipefail

# Model a multi-phase Rust gate without building or running any Rust code.
# Cargo progress uses stderr; the script's phase labels and verdict use stdout.
for phase in 1 2; do
  printf '==> cargo test --locked --workspace --quiet (phase %s)\n' "$phase"
  for index in {1..40}; do
    printf '   Compiling gate_dep_%s_%s v0.1.0\n' "$phase" "$index" >&2
  done
  printf '    Finished `test` profile [unoptimized + debuginfo] target(s) in 1.00s\n' >&2
  printf 'running 75 tests\n'
  for index in {1..75}; do
    printf 'test phase_%s::case_%s ... ok\n' "$phase" "$index"
  done
  printf 'test result: ok. 75 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n'
  printf '    ok (1s)\n'
done

printf '==> cargo nextest run --workspace\n'
printf '    Starting 80 tests across 1 binary\n'
for index in {1..80}; do
  printf '        PASS [   0.001s] gate::case_%s\n' "$index"
done
printf '     Summary [   0.080s] 80 tests run: 80 passed, 0 skipped\n'
printf '    ok (1s)\n'
printf '==> generated files untouched by the build\n'
printf '    ok (0s)\n'
printf 'GATE PASSED: all phases green\n'
