# aft_search fusion quality investigation (R3-A / #252)

## Current fusion mechanics

Source read: `crates/aft/src/commands/semantic_search.rs`, `crates/aft/src/search_index.rs`, `crates/aft/src/semantic_index.rs`, and `crates/aft/src/query_shape.rs` at `3f9256457968e22382d8dfc3f796ed5526f0344e`.

- Auto mode routes non-NL identifier/path/error/mixed queries to Hybrid when the lexical index is ready; long natural-language queries stay semantic-only (`semantic_search.rs:159-223`).
- Semantic lane: the query is embedded, then `SemanticIndex::search` returns `semantic_candidate_limit(top_k)` candidates (`top_k * 3`, clamped 10..100) plus one for `more_available` (`semantic_search.rs:153-157`, `495-514`). Each chunk score is cosine similarity, multiplied by `1.1` when the chunk is exported (`semantic_index.rs:2103-2153`).
- Semantic chunks include symbol name/file/kind/signature/body (`semantic_index.rs:2883-2946`). File-summary chunks are also added for files with <=2 top-level exports and include file path, parent, leading doc, and export names (`semantic_index.rs:2996-3048`, `3188-3266`).
- Lexical lane: hybrid collection tokenizes the query, converts tokens to trigrams, and asks `SearchIndex::lexical_rank_with_stats(..., LEXICAL_ENUMERATION_LIMIT=50)` (`semantic_search.rs:1038-1075`). The trigram ranker uses up to the three rarest non-zero query trigrams, caps pre-rank candidates at 200/500, then scores a file as `hits / (1 + ln(file_trigram_count))` (`search_index.rs:104-173`, `1579-1605`).
- Fusion is score sorting, not true RRF. If a semantic result's file is in the lexical map, the final score is only `semantic_score * HYBRID_LEXICAL_BOOST` (`1.1`); the lexical score is recorded but not added. Lexical-only files get `min(lexical_score * shape_weight, 0.25)`, where `shape_weight` is `0.8` for identifiers and `0.5` for path/error/mixed (`semantic_search.rs:1112-1234`). Results are sorted by this score, capped to two entries per file, sorted again, then truncated.

Implication: an exact lexical-only identifier hit cannot score above `0.25`. Any weakly related semantic chunk above `0.25` outranks it, and even a file present in both lanes only receives a flat 10% semantic multiplier. The classifier's shape weights (`query_shape.rs:694-722`) are not used as a normalized semantic/lexical weighted sum in the current fusion path.

## Benchmark additions

Added:

- `benchmarks/aft-search/identifier-fusion-fixtures.json` — 13 exact identifier/constant/token queries chosen to have a verbatim target file and plausible semantic distractors.
- `benchmarks/aft-search/run-fusion-quality` — runs the focused set plus existing `fixtures.json`, requests `semantic_search(top_k=100)`, and compares current order with three bench-only rerankers.
- `benchmarks/aft-search/results/search-fusion-quality.json` and `search-fusion-quality-summary.tsv` — committed run output.

Reproduce from `benchmarks/aft-search`:

```bash
python3 run-fusion-quality \
  --binary ../../target/release/aft \
  --project-root ../.. \
  --out results/search-fusion-quality.json \
  --summary results/search-fusion-quality-summary.tsv
```

Run metadata: `aft 0.37.0`, fastembed `all-MiniLM-L6-v2`, 384 dimensions, 11,954 semantic entries, 1,393 lexical files, 72,898 trigrams. The script auto-used cached ONNX Runtime at `~/.local/share/cortexkit/aft/onnxruntime/1.24.4/libonnxruntime.dylib`. One existing golden expected file is stale (`crates/aft/src/imports.rs`); the script records it but still evaluates the remaining expected file for that query.

## Results

Rates are file-level. The run asks for a wide top-100 candidate pool so buried exact files are observable; R@5 below is whether a target file is in the first five positions of that returned order.

| Suite | Candidate | R@1 | R@5 | MRR | Query p50/p95 | Rerank p95 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| focused identifier (13) | current | 46.2% | 69.2% | 0.555 | 8.6 / 57.0 ms | 0.011 ms |
| focused identifier | RRF + exact lane | 69.2% | 69.2% | 0.719 | same | 0.193 ms |
| focused identifier | exact identifier first | 100.0% | 100.0% | 1.000 | same | 0.028 ms |
| focused identifier | uncapped identifier lexical | 23.1% | 92.3% | 0.543 | same | 0.062 ms |
| existing golden (28) | current | 53.6% | 71.4% | 0.619 | 7.2 / 8.4 ms | 0.010 ms |
| existing golden | RRF + exact lane | 39.3% | 71.4% | 0.536 | same | 0.145 ms |
| existing golden | exact identifier first | 50.0% | 75.0% | 0.601 | same | 0.027 ms |
| existing golden | uncapped identifier lexical | 35.7% | 57.1% | 0.465 | same | 0.060 ms |

Failure examples from the focused set:

- `DEFAULT_READY_TIMEOUT_SECS`: target `benchmarks/aft-search/run.py` ranked 88th. Top results were timeout-related semantic chunks (`timeout_ms`, `DEFAULT_TIMEOUT_MS`, `pull_file_timeout`) with lexical scores, all above the lexical-only ceiling.
- `VOLATILE_TOP_LEVEL_KEYS`: target ranked 46th behind cache/key metadata functions.
- `subagent_type`: exact classifier fixture ranked 93rd behind subagent-detection code.
- `aft_safety_history`: exact tokenizer/classifier fixtures ranked 93rd behind AFT bridge/doctor code.
- `LEXICAL_ONLY_SCORE_CEILING`: target file was top-5 but not #1; lexical-rank functions outranked the exact fusion constant.

## Candidate fix tradeoffs

1. **True-ish RRF between semantic rank and an exact lexical lane** improved focused R@1 (46.2% -> 69.2%) but did not recover focused R@5 and regressed existing-golden MRR/R@1. RRF alone is not enough when semantic rank-1 and lexical rank-1 ties are common, and it can pull exact-but-not-golden files upward.
2. **Exact identifier first** fixed the focused failures and modestly improved existing-golden R@5 (71.4% -> 75.0%), with negligible offline rerank overhead. But it slightly regressed existing R@1/MRR because raw substring exactness promotes tests/docs/alternate plugin copies for some queries (`LSPManager`, `useState`).
3. **Uncapping identifier lexical score** improved focused R@5 but badly regressed the existing suite. Broad trigram overlaps become too strong when the 0.25 ceiling is simply removed.

## Recommendation

Do not ship a blind uncapped lexical lane. The safest direction is a calibrated exact-identifier guardrail: for identifier-shaped queries, boost files where the exact token matches a symbol name/path or a high-confidence lexical occurrence, but avoid raw substring-first promotion of tests/docs/benchmark fixtures. Pair that with rank-aware fusion (RRF or weighted rank blend) only after the exact-match signal is made precise. This preserves the strong focused-set recovery shown by `exact_identifier_first` while addressing the observed existing-suite MRR regressions.
