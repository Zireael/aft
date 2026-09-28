# Preliminary dev baseline (before optimization)

These are diagnostic measurements, not release latency estimates. Host load was
191/240/318 at one observation. Counts exclude corpus construction. Initial
real-corpus work counts used the working source tree; the frozen-corpus replay
is the authority for final before/after comparisons.

| AFT lane/case | Dev ms | Allocations | Requested bytes | Posting materializations | Source reads | Literal regex compilations | Scores | Sort inputs |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| lexical / natural language | 2402.057 | 141130 | 39262472 | 184 | 0 | 0 | 118231 | 118721 |
| lexical / identifier | 236.829 | 20850 | 5869770 | 28 | 0 | 0 | 15870 | 16619 |
| lexical / common + rare | 1174.212 | 36707 | 10281118 | 50 | 0 | 0 | 29184 | 30064 |
| exact / natural language | 23370.694 | 2042209 | 415931793 | 141 | 2101 | 0 | 0 | 0 |
| exact / identifier | 25184.054 | 1432132 | 376004379 | 24 | 1295 | 0 | 0 | 0 |
| exact / common + rare | 15666.942 | 541487 | 231548039 | 47 | 836 | 0 | 0 | 0 |
| anchored / literal `search.*index` | 45506.443 | 6567542 | 2381163624 | 0 | 3635 | 3624 | 0 | 0 |

The anchored row invokes the anchored lane directly: `.*` is literal text to
its run verifier, not a regex wildcard. The standalone script now separately
asserts real log-excerpt routing (`runtime WARN search 42 index`) rather than
mistaking the regex router's fallback walk for anchoring.

| Controlled case | Dev ms | Allocations | Requested bytes | Work |
|---|---:|---:|---:|---|
| lexical exactness, distant tokens | 73.608 | 12445 | 495918 | 3003 joined-window token scans |
| lexical exactness, missing token | 306.831 | 18003 | 565779 | 2997 joined-window token scans |
| semantic, 8000 chunks / 200 files | 287.953 | 202 | 143717 | 8000 score evaluations |
| semantic, borrowed same corpus | 153.431 | 16252 | 1564844 | 8000 predicate calls |
| semantic refresh, 1 unchanged file | 66.492 | 1911 | 453391 | 1 source read |
| semantic refresh, 100 unchanged files | 490.172 | 189151 | 45426594 | 100 source reads |

The first semantic fixture used one-line bodies and produced zero admitted
chunks; its explicit `results.len() == 50` assertion failed. It was corrected to
multiline functions with an explicit 8000-chunk admission assertion before any
semantic optimization. No zero-work semantic measurement is accepted.

Material mechanisms selected from these counts: lexical repeated discovery
ranking (PERF-01), overlapping exact-window allocation and absent-token scans
(PERF-04/05), anchored compilation and empty-window construction (PERF-07/08),
borrowed path/filter repetition (PERF-14), and refresh reuse-map payload copies
and second traversal (PERF-15, the reuse-map portion of PERF-17). Deferred
refresh admission is tested separately at full capacity (PERF-16).

Not selected: PERF-02's final canonical sort is small relative to the removed
per-trigram sorts in these cases; PERF-13's 128 KB score scratch for 8000 chunks
is small compared with borrowed path allocation and retaining it avoids adding
an O(log K) heap operation per vector. PERF-03 cannot intersect token postings
without losing any-token definition evidence. PERF-06 requires a separate
source freshness/decoding contract; cross-lane caching is not introduced.
PERF-17's returned delta still owns a copy because the refreshed index and its
consumer both need the entries. No persisted format change is introduced.

## Baseline and harness caveats

- The first standalone attempt indexed this task worktree with empty isolated
  storage and remained `loading` for 1800 seconds. Worktree artifact borrowing
  is read-only; the corrected harness archives the pinned corpus into an
  independent temporary root and succeeds with isolated storage.
- Initial exact recall timed out on a 30-second status RPC under dev-profile
  host contention. `AFT_SEARCH_BENCH_RPC_TIMEOUT=600` explicitly extends only
  client waiting; repeated exact/concept benchmarks then completed with all
  ranked rows unchanged.
- A baseline `single_page` search-quality attempt completed exact and concept
  recall but failed `capability_probe_inconsistency`: the current schema declares
  offset support, while the single-page runner does not populate the required
  `probe_pages_differ` capability. The authoritative check uses `paged`; this
  unrelated gate issue is not fixed or waived by the train.
- The supplemental `search.*index` fallback query has different capped rows
  across calls on the same before-binary. Its actual diff is in
  `HOT_PATH_FALLBACK_INSTABILITY.md`. The owner explicitly authorized continuing
  required-lane parity with only volatile health/repetition footers removed.
