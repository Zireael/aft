# Search hot-path work-count harness

Use the **dev profile** on the same machine for before/after comparisons. Wall
clock values include host scheduling and must not be presented as release
latencies. Work counts are the primary evidence.

```bash
CARGO_BUILD_RUSTC_WRAPPER= RUSTC_WRAPPER= cargo build -p agent-file-tools --bin aft -j 2
python3 benchmarks/aft-search/run_hot_path.py --binary target/debug/aft \
  --corpus "$PWD" --corpus "$HOME/Work/OSS/opencode" \
  --out benchmarks/aft-search/.bench/hot-path-before.json
bash benchmarks/aft-search/run_hot_path_counts.sh "$PWD"
bash benchmarks/aft-search/run_hot_path_counts.sh "$HOME/Work/OSS/opencode"
```

The Python runner starts a standalone AFT for each real corpus with a fresh
`TemporaryDirectory` storage root. It runs fixed natural-language, code literal,
common-plus-rare-token and anchored queries, recording each complete response
and per-request elapsed time. `--semantic` enables the installed embedding
backend; without it the standalone run measures lexical/exact/anchored paths.
First requests and subsequent memo hits are separate rows, not pooled averages.

The Rust runner indexes real files of at most 1 MiB using the ignore-aware
walker, then measures the lexical, exact and anchored lanes independently.
`HOT_PATH` JSON lines report allocations, posting materializations, source read
attempts, regex compilations, score evaluations and sort input sizes. Counters
are test-only; ordinary builds have no added counter calls. Posting counts are
materializations, not physical device reads. Sort input counts include all
instrumented lexical rank sorts plus final canonical sorting. Read counters
exclude corpus setup, metadata probes and unrelated parser internals.

Two additional ignored tests measure missing/distant-token exactness and a
controlled 200-file, 40-functions-per-file semantic corpus with 384-dimensional
deterministic vectors. Those vectors are **not** a ranking-quality substitute.
The semantic test measures search, refreshes of 1 and 100 files (unchanged text,
so embedding reuse is exercised), then borrowed-base filtering. Process-wide
allocation counting includes Rayon workers; run with `--test-threads=1` and do
not mix these ignored tests with unrelated work in the same test process.

The independent ranking gates must use the actual dev binary:

```bash
AFT_BINARY_PATH="$PWD/target/debug/aft" scripts/telemetry/cost-gate.sh \
  --search-quality --mode record-reference --dry-run
python3 benchmarks/aft-search/run_exact_recall.py --binary "$PWD/target/debug/aft" \
  --out benchmarks/aft-search/.bench/hot-path-exact.json
AFT_BINARY_PATH="$PWD/target/debug/aft" python3 benchmarks/aft-search/run_concept_recall.py \
  --output benchmarks/aft-search/.bench/hot-path-concept.json
```

Read the benchmark README's macOS FSEvents warning before interpreting semantic
row differences. A changing row is not permission to update the baseline.

## Source checks before optimization

- PERF-01: confirmed per-trigram calls to the complete lexical ranker, followed
  by an all-trigram rank and path-set restriction. Membership cardinality ties
  use the trigram value, not the ranker's stable input order.
- PERF-02: confirmed full sort/dedup before the 3,200-candidate cap. Any partial
  selection must preserve the winning duplicate's evidence and selected-pool
  exhaustion count, not merely its path.
- PERF-03: the union is also needed for definition evidence: the exact verifier
  accepts a symbol whose name equals **any** content token. Intersecting all
  token postings would discard valid definitions even when E2 cannot match.
- PERF-04: confirmed repeated window joins/token-set construction after the
  whole-file substring precheck.
- PERF-05: confirmed missing-token lexical window scans. The exact precheck
  cannot be reused verbatim: it requires two tokens, whereas lexical exactness
  currently accepts a single token. Rejecting single-token windows would change
  results.
- PERF-06: confirmed independent source reads in lexical exactness, exact corpus
  verification and anchored verification. A cache must retain each lane's
  admission/decoding behavior and must not hide mutations between reads without
  explicitly defining a request snapshot contract.
- PERF-07/08: confirmed full per-window occurrence filtering and repeated
  escaped case-insensitive literal matcher compilation for every candidate.
- PERF-13/14: confirmed corpus-sized score scratch storage and repeated borrowed
  path joins/predicate calls per chunk. Comparator index tie-breaking and
  cancellation checks every 64 entries must remain unchanged.
- PERF-15/17: confirmed full-entry reuse/removal scans and cloned refresh delta.
  Both the refreshed index and returned delta own their entries; removing the
  clone without changing ownership would break the apply/update contract.
- PERF-16: admission is based on **successful** collection, not merely requested
  paths. Failed changes free capacity for new files. An early cap based on
  requested counts would change which files are admitted.
