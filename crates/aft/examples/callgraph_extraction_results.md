# Callgraph blob extraction work census

## Inputs and method

The immutable inputs are tracked regular files from AFT revision
`be561ad598bba4576ae92fac9b9e0813b9233b06` and opencode revision
`5716f8ba60e79ec60ec485b6e5291c0b0bc1f252`. Supported extensions are selected by
`parser::detect_language`. Both extractors receive the same source bytes,
language label and `work-census-v1` extractor-version string.

The baseline is an archived workspace with test-only instrumentation, frozen
before implementation. Production extraction logic is unchanged in that
workspace. Parser invocations are intercepted by a test-only parser wrapper;
restricted Rust macro-body ranges are recorded separately from whole-file
parses. Node-kind copies and child-vector collections are counted at their
construction sites. Baseline ordinal and dispatch counters count candidate
visits; indexed lookup counters count consumed nodes, binary-search comparisons
and Fenwick bucket visits. Position counters count source bytes scanned by
position helpers and index construction, including call attribution and
structural references. Child-vector collections include empty vectors, so they
are not a count of heap allocations. The allocation tests additionally exercise
cursor traversal and call-kind lookup through the existing counting allocator.

## Baseline measurements

These are debug/test-profile, instrumented runs. Their wall times are contextual
measurements, not an idle-machine performance claim.

| File | Source bytes / lines | AST nodes | Whole-file / macro parses | Kind copies | Child-vector collections | Ordinal visits | Dispatch visits | Position bytes scanned | Call-kind vectors | Extract wall time |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| AFT `crates/aft/src/subc/mod.rs` | 589,914 / 14,841 | 125,838 | 6 / 0 | 125,838 | 251,676 | 975,370,338 | 468,566,856 | 195,576,446 | 468,704,385 | 84.231 s |
| AFT `packages/opencode-plugin/test/load-matrix/load-matrix.ts` | 119,139 / 2,999 | 21,193 | 4 / 0 | 21,193 | 21,193 | 24,647,459 | 12,066,552 | 3,474,829 | 12,089,669 | 1.945 s |
| opencode `packages/client/src/promise/generated/types.ts` | 215,570 / 6,265 | 66,653 | 4 / 0 | 66,653 | 66,653 | 50,722,933 | 0 | 116,879,920 | 72,511 | 4.629 s |

Baseline corpus extraction produced 3,526 AFT blobs (1,076,172,901 serialized
bytes, 2,142.68 s) and 5,037 opencode blobs (931,275,084 serialized bytes,
463.64 s). Every supported regular file in each snapshot was extracted.

The baseline work-budget run failed the six expected guards: whole-file parse
count, kind copies, child vectors, position scans, call-kind vectors and lookup
growth. The two-item-macro guard passed. Doubling the synthetic TypeScript file
from 64 to 128 functions increased ordinal plus dispatch visits from 561,472 to
2,245,248 (approximately 4×).

## Head measurements

The same three immutable source files were measured with the optimized extractor
before starting the corpus comparison.

| File | Whole-file / macro parses | Kind copies | Child-vector collections | Ordinal index visits | Dispatch index visits | Position bytes scanned | Call-kind vectors | Extract wall time |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| AFT `crates/aft/src/subc/mod.rs` | 1 / 0 | 0 | 0 | 3,680,814 | 345,002 | 589,914 | 0 | 7.886 s |
| AFT `packages/opencode-plugin/test/load-matrix/load-matrix.ts` | 1 / 0 | 0 | 0 | 513,997 | 46,596 | 119,139 | 0 | 0.209 s |
| opencode `packages/client/src/promise/generated/types.ts` | 1 / 0 | 0 | 0 | 1,759,295 | 0 | 215,570 | 0 | 0.315 s |

The head work-budget run passed: 13 tests passed, none failed, and one corpus
probe was ignored. The filter includes four existing extraction-related tests
as well as the nine new tests. Synthetic lookup work grew from 47,743 to
103,822 visits for 64 to 128 functions (approximately 2.17×). The macro fixture
asserted exactly two range parses with distinct ranges and retained both symbols.

These wall times are instrumented debug measurements under fleet load. An
idle-ish timing run was not completed before the comparison failure.

## Implementation

One caller-owned whole-file tree is shared by call-data extraction, nested Rust
imports, Rust modules, member completion and dispatch. Necessary item-position
macro-body range parses are preserved. AST kinds borrow static grammar names;
decoding owns those names without changing JSON. Traversal uses cursors, and
smallest-enclosing-range queries use an offline start sweep with reversed-end
Fenwick minima. A shared position index retains both historical column-clamping
conventions. Call-kind lists are static slices.

At the baseline revision, `collect_reexport_refs` scans source text rather than
parsing a tree. Dispatch, however, allocates call-kind vectors inside node and
reference-candidate predicates, not merely once per file.

## Byte comparison failed: verification stopped

The AFT corpus comparison stopped at
`benchmarks/aft-search/author_real_query.py`. Baseline serialization has 319,954
bytes; head serialization has 319,970 bytes. Inspection of the saved payloads
found exactly two differing JSON values:

| JSON path | Baseline | Head |
| --- | --- | --- |
| `$.refs[29].caller_symbol` | `at_pin` | `pinned_domains` |
| `$.refs[30].caller_symbol` | `at_pin` | `pinned_domains` |

`at_pin` is nested inside `pinned_domains`. Extraction still iterates
`calls_by_symbol`, a hash map, and reference sorting and deduplication still omit
`caller_symbol` from their keys. That unchanged behavior is a plausible source
of nondeterministic selection between enclosing callers, but a repeat-baseline
experiment has **not** been run: extraction and verification were stopped at the
first mismatch as required. Byte equivalence is not established.

No extractor-version bump or reference canonicalization was attempted. The
opencode comparison, mutation controls, callgraph/views suites and Windows check
were not run after this failure. This implementation remains a failed acceptance
candidate, not a completed optimization.

## Commands and retained evidence

Toolchain: Cargo 1.99.0, rustc 1.99.0, rustfmt 1.10.0-stable. Cargo commands use
an explicit three-hour timeout. The baseline commands run from
`tmp/extraction/base` with `CARGO_TARGET_DIR` set to the worktree's `target/`.

- `cargo test -p agent-file-tools --lib extraction_ -- --nocapture`: baseline
  produced six expected budget failures; head passed 13 tests with one ignored.
- `cargo test -p agent-file-tools --lib extraction_corpus_bytes_and_work -- --ignored --nocapture`:
  baseline passed once for each full corpus; head passed once for each of the
  three single-file work probes.
- The same corpus command with `AFT_EXTRACT_COMPARE=1`: failed on the AFT corpus
  with the mismatch above.
- `cargo fmt --all -- --check`: passed (silent success).

Ignored evidence is retained under `tmp/extraction/`: `baseline-tests-resumed.log`,
`baseline-aft-resumed.log`, `baseline-opencode-resumed.log`,
`head-work-tests-fixed.log`, `head-subc-work.log`, `head-load-matrix-work.log`,
`head-opencode-work.log`, `head-aft-byte-comparison.log`, and
`byte-mismatch-fields.txt`. The differing payloads are
`baseline-aft/benchmarks/aft-search/author_real_query.py` and
`baseline-aft/benchmarks/aft-search/author_real_query.head.json`.
