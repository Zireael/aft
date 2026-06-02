# CodeGraph benchmark replication plan for AFT

## 1. Metrics to replicate

Replicate the deterministic, no-LLM retrieval-quality eval from `codegraph/__tests__/evaluation/`:

- **Recall** over expected symbols, using the same pass rule as CodeGraph (`recall >= 0.5`).
- **MRR** from the first ranked result that matches an expected symbol or expected file.
- **Precision@k** for k = 1, 5, 10. CodeGraph's current scorer does not expose P@k, but the user asked for it and it is compatible with the same ranked result list.
- **Found/missed symbols** per case.
- **Real wall-clock latency** around the actual tool dispatch. The report will include per-case latency samples plus median and p95 latency at driver summary level. With `--runs > 1`, each query gets per-query median/p95; with the default single run those values equal the single dispatch time.

Keep CodeGraph's `nodeCount`, `edgeCount`, and `edgeDensity` fields optional. AFT's retrieval tools do not expose graph edge counts for `aft_search`, `grep`, or ripgrep, so those fields will remain absent instead of fabricated.

## 2. Corpus choice

Use three corpus sources:

1. **`codegraph` (default for apples-to-apples AFT runs):** an AFT-side translation of CodeGraph's 12 test-case shapes. It preserves CodeGraph's split between exact symbol lookup (`searchNodes`) and broader context exploration (`findRelevantContext`), but rewrites Elasticsearch-specific symbols (`TransportService`, `RestController`, etc.) to equivalent symbols in this repository (`BinaryBridge`, `BridgeOptions`, `handle_semantic_search`, etc.). Each rewritten case records its `sourceCaseId` and a note explaining the substitution.
2. **`codegraph-original`:** a JSON copy of the exact CodeGraph structured corpus. This is useful when someone points the harness at Elasticsearch or another checkout containing those symbols. It is expected to fail or be skipped on `opencode-aft`, so it is not the default run for this repo.
3. **`aft`:** small AFT-native supplemental cases for tool-surface coverage that CodeGraph does not have one-to-one (outline/zoom/navigate-oriented cases). Custom corpus files can also be loaded by path with the same schema.

This keeps the publishable comparison honest: `codegraph-original` is the literal upstream corpus; `codegraph` is the translated corpus used to run the same methodology against AFT itself.

## 3. Tool mapping

| CodeGraph eval API/tool | AFT equivalent in this harness | Notes |
| --- | --- | --- |
| `searchNodes(query, { limit, kinds })` | `aft_search` (`semantic_search` bridge command with `top_k`) | Use symbol/file/kind metadata from AFT hybrid results. `kinds` is retained as corpus metadata and reported, but AFT does not currently filter semantic search by kind. |
| `findRelevantContext(query, { searchLimit, traversalDepth, maxNodes })` | `aft_search` by default; optional corpus cases may request `aft_outline`, `aft_zoom`, or `aft_navigate` | AFT has separate focused tools instead of one subgraph-returning context API. For apples-to-apples scoring, the ranked retrieval result is still normalized into the same item list. |
| CodeGraph `node`/source inspection | `aft_zoom` | Only for cases with explicit `file` + `symbol`; not used for broad search scoring by default. |
| CodeGraph `context`/file overview | `aft_outline` | Useful for AFT-specific supplemental cases. Outline text is normalized into file/symbol-ish result items when possible. |
| CodeGraph `trace`/call graph | `aft_navigate` commands (`callers`, `call_tree`, `trace_to_symbol`, etc.) | Only measured for explicit navigate cases; graph edge density is not scored. |
| Plain lexical baseline | AFT bridge `grep` and external `rg -F` | Both use real wall-clock dispatch and fixed-string lexical matching. |
| Sanity baseline | List files only | Ranks file paths without looking at query text; proves the scorer is not trivially passing. |

## 4. What will not be replicated

- **Agent A/B matrix** (`scripts/agent-eval/`, tmux/Claude runs, token/cost/tool-call behavior): explicitly out of scope for this task and depends on harness machinery AFT does not have here.
- **Graph edge metrics** (`edgeCount`, `edgeDensity`) for non-graph AFT drivers: AFT does not expose a CodeGraph-style returned subgraph for `aft_search`, AFT grep, ripgrep, or list-files. Reporting zero would be misleading, so those fields stay omitted.
- **Kind-filtered semantic retrieval:** CodeGraph can pass `kinds` into `searchNodes`; AFT's semantic search does not accept a kind filter today. Kinds are used only for metadata/diagnostics.
- **AFT `aft_search` vs CodeGraph on Elasticsearch in this commit:** the harness supports `codegraph-original`, but the verification run for this task is against `opencode-aft` because that is the indexed local target.

## 5. Output format

Emit JSON close to CodeGraph's `EvalReport`:

```ts
{
  timestamp: string,
  codebasePath: string,
  codegraphSha: string,
  aftSha?: string,
  benchmark: "codegraph-replication",
  corpus: string,
  driver: string,
  summary: {
    total: number,
    passed: number,
    failed: number,
    skipped: number,
    meanRecall: number,
    meanMRR: number,
    meanPrecisionAt1: number,
    meanPrecisionAt5: number,
    meanPrecisionAt10: number,
    latencyMsMedian: number,
    latencyMsP95: number
  },
  results: EvalResult[]
}
```

`EvalResult` keeps CodeGraph-compatible fields (`caseId`, `pass`, `recall`, `mrr`, `foundSymbols`, `missedSymbols`, `latencyMs`) and adds ranked `results`, `precisionAtK`, `driver`, `api`, and optional `skipReason`. A markdown summary with the same aggregate table and per-case rows will be written beside the JSON so results can be pasted into docs/README.

## 6. code-review-graph patterns borrowed

I also read `/Users/ufukaltinok/Work/OSS/code-review-graph/code_review_graph/eval/` for methodology inspiration. This benchmark will still replicate CodeGraph first, but borrows these low-cost patterns where they improve reproducibility without adding dependencies on that project:

- **Pinned repo metadata shape:** corpus entries can carry repo name, URL, language, size category, and pinned commit fields, matching code-review-graph's `configs/*.yaml` discipline. v1 runs against `opencode-aft`, but this schema lets us add the reusable `fastapi`, `flask`, `gin`, `express`, `httpx`, and `code-review-graph` repos later without redesign.
- **Separated task axes:** keep CodeGraph's `searchNodes` vs `findRelevantContext` API labels, but also tag cases with categories analogous to code-review-graph's `search_queries` and `multi_hop_tasks` so later reports can split symbol lookup, context exploration, and navigation/multi-hop retrieval.
- **Deterministic reporting:** include corpus path, codebase SHA, AFT binary path, driver, top-k, and runs in every report. This mirrors code-review-graph's pinned-SHA/config-driven reproducibility while keeping the AFT harness simple.
- **Real wall-clock timing per dispatch:** code-review-graph times build/search stages directly; AFT will time the actual bridge or process dispatch around each query and aggregate median/p95.
- **Token accounting is deferred:** code-review-graph's tiktoken-calibrated token-efficiency axis is useful, but it belongs to a broader agent/context benchmark, not this no-LLM CodeGraph retrieval replication. v1 may record result payload sizes later, but will not mix token-efficiency scores into retrieval quality.

Patterns intentionally not borrowed for v1: the six-axis suite (`impact_accuracy`, `multi_hop_retrieval`, `search_quality`, `token_efficiency`, `flow_completeness`, `build_performance`) and repository cloning/build orchestration. Those are valuable follow-on axes, but this deliverable stays focused on deterministic retrieval scoring against AFT's actual tool surface.

