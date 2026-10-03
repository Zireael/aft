# Search-quality reference re-recorded after trains 275 and 276

Train 280 changed two ranking-fenced files: `semantic_index.rs` and `rerank/remote.rs`. In both, embedding and rerank calls now share one HTTP client, and each request keeps its own timeout. It was classed `engine_unwired`, so CI byte-compared its benchmark rows with the committed reference. The comparison failed with `engine_unwired_mismatch` on row `real_query.followup-census:3184`.

The diff showed that the committed reference predated trains 275 (split query, which added `aft_search`'s `pattern` parameter) and 276 (the definition-line, source-over-docs, `receiver.member` and nearest-name fixes). Both were `ranking` trains. A ranking landing passes the gate, but it never re-records the reference, and the trains in between (277–279) touched no fenced file, so the comparison never ran. This is the same gap as train 114.

The reference was re-recorded with `AFT_BINARY_PATH=<release aft> scripts/telemetry/cost-gate.sh --search-quality --mode record-reference`, on a release build of `origin/main` at `5cb844c96` (train 279, which contains 275 and 276), in a separate worktree, with the benchmark corpus provisioned by `provision_corpus.py`:

| Field | Value |
|---|---|
| Real-query rows | 93 |
| hit@1 | 0.301075 |
| hit@5 | 0.526882 |
| MRR@10 | 0.394892 |

These equal the scores train 280's head produced on CI against the stale reference (0.301075 / 0.526882 / 0.394892). So train 280's engine and main's engine rank identically on the full set, as `engine_unwired` claims.

From now on, the first item of the train after any `ranking` landing is this re-record.
