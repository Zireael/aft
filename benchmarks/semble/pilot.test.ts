import { describe, expect, it } from "bun:test";
import {
  aggregateMetrics,
  buildEmptyCounts,
  buildLexicalQueriesFromCanon,
  buildFeatureBranchComparison,
  buildSemanticRuns,
  formatChunkSizeLog,
  groupLexicalQueriesByRepo,
  identifierModePlan,
  applyLegacySnippetCap,
  ndcgAtK,
  shouldRunIdentifierSemantic,
  symbolResultFromFts5Row,
  splitAggregatesBySuite,
  type ModeResult,
} from "./pilot";

describe("pilot benchmark reporting", () => {
  it("averages recall over every eligible query, including empty attempts", () => {
    const rows: ModeResult[] = [
      { mode: "fts5", query: "q1", repo_name: "serde", category: "semantic_nl", suite: "semantic_nl", latency_ms: 10, results: [{ file: "a.rs" }], recall_at_k: 1, mrr: 1, ndcg_at_k: 1 },
      { mode: "fts5", query: "q2", repo_name: "serde", category: "semantic_nl", suite: "semantic_nl", latency_ms: 10, results: [{ file: "b.rs" }], recall_at_k: 1, mrr: 1, ndcg_at_k: 1 },
      { mode: "fts5", query: "q3", repo_name: "serde", category: "semantic_nl", suite: "semantic_nl", latency_ms: 10, results: [], recall_at_k: 0, mrr: 0, ndcg_at_k: 0 },
    ];

    const aggregate = aggregateMetrics(rows, 3);

    expect(aggregate.recall).toBeCloseTo(2 / 3, 5);
    expect(aggregate.count).toBe(3);
    expect(aggregate.empty).toBe(0);
  });

  it("keeps semantic and identifier aggregates in separate report sections", () => {
    const rows: ModeResult[] = [
      { mode: "rg", query: "natural language", repo_name: "serde", category: "semantic_nl", suite: "semantic_nl", latency_ms: 10, results: [], recall_at_k: 0, mrr: 0, ndcg_at_k: 0 },
      { mode: "rg", query: "Serialize", repo_name: "serde", category: "identifier_exact", suite: "identifier_exact", latency_ms: 10, results: [{ file: "serde_core/src/ser/mod.rs" }], recall_at_k: 1, mrr: 1, ndcg_at_k: 1 },
    ];

    const split = splitAggregatesBySuite(rows, { semantic_nl: 1, identifier_exact: 1 });

    expect(split.semantic).toHaveLength(1);
    expect(split.lexical).toHaveLength(1);
    expect(split.semantic[0].mode).toBe("rg");
    expect(split.semantic[0].recall).toBe(0);
    expect(split.lexical[0].mode).toBe("rg");
    expect(split.lexical[0].recall).toBe(1);
  });

  it("builds lexical queries from checked-in canon suites, not stale hard-coded repos", () => {
    const queries = buildLexicalQueriesFromCanon("benchmarks/semble/canon", new Set(["serde"]));

    expect(queries.length).toBeGreaterThan(0);
    expect(queries.every((q) => q.repos.includes("serde"))).toBe(true);
    expect(queries.some((q) => q.query === "Serialize" && q.suite === "identifier_exact")).toBe(true);
    expect(queries.some((q) => q.query === "Ser" && q.suite === "identifier_prefix")).toBe(true);
  });

  it("reports empty attempts by suite and mode", () => {
    const rows: ModeResult[] = [
      { mode: "fts5", query: "q1", repo_name: "serde", category: "semantic_nl", suite: "semantic_nl", latency_ms: 10, results: [], recall_at_k: 0, mrr: 0, ndcg_at_k: 0, status: "empty" },
      { mode: "fts5", query: "q2", repo_name: "serde", category: "identifier_exact", suite: "identifier_exact", latency_ms: 10, results: [], recall_at_k: 0, mrr: 0, ndcg_at_k: 0, status: "empty" },
      { mode: "fts5", query: "q3", repo_name: "serde", category: "identifier_exact", suite: "identifier_exact", latency_ms: 10, results: [{ file: "a.rs" }], recall_at_k: 1, mrr: 1, ndcg_at_k: 1, status: "ok" },
    ];

    expect(buildEmptyCounts(rows)).toEqual({
      "identifier_exact/fts5": 1,
      "semantic_nl/fts5": 1,
    });
  });

  it("does not award nDCG credit to blank result paths", () => {
    expect(ndcgAtK([{ file: "" }], ["serde/src/lib.rs"], 5)).toBe(0);
  });

  it("normalizes exact SQL symbol rows after file-path fallback resolution", () => {
    expect(symbolResultFromFts5Row(
      { symbol_id: 42, file_id: 7, start_line: 10, end_line: 12, symbol_name: "Serialize" },
      "serde_core/src/ser/mod.rs",
    )).toEqual({
      file: "serde_core/src/ser/mod.rs",
      line: 10,
      score: undefined,
      symbol_id: 42,
    });
  });

  it("groups lexical canon queries by repo so sessions initialize once per repo", () => {
    const queries = buildLexicalQueriesFromCanon("benchmarks/semble/canon", new Set(["serde", "axum"]));
    const grouped = groupLexicalQueriesByRepo(queries);

    expect(grouped.map(([repo]) => repo)).toEqual(["axum", "serde"]);
    for (const [, repoQueries] of grouped) {
      expect(new Set(repoQueries.map((q) => q.repo_name)).size).toBe(1);
      expect(repoQueries.some((q) => q.suite === "identifier_exact")).toBe(true);
      expect(repoQueries.some((q) => q.suite === "identifier_prefix")).toBe(true);
    }
  });

  it("runs dedicated symbol lookup modes for identifier suites", () => {
    expect(identifierModePlan("identifier_exact", ["model2vec"], false)).toEqual([
      "lexical (rg)",
      "aft-grep",
      "fts5",
      "fts5_find_symbol_exact",
    ]);
    expect(identifierModePlan("identifier_prefix", ["fastembed"], false)).toEqual([
      "lexical (rg)",
      "aft-grep",
      "fts5",
      "fts5_find_symbol_prefix",
    ]);
  });

  it("keeps semantic identifier comparison explicit opt-in", () => {
    expect(shouldRunIdentifierSemantic("quick", undefined)).toBe(false);
    expect(shouldRunIdentifierSemantic("full", undefined)).toBe(false);
    expect(shouldRunIdentifierSemantic("full", true)).toBe(true);
    expect(identifierModePlan("identifier_exact", ["model2vec", "fastembed"], true)).toContain("semantic-fe");
  });

  it("omits over2048=0 from normal verbose chunk logs and warns only for oversized chunks", () => {
    expect(formatChunkSizeLog("sem-model2vec", ["small chunk"])).toEqual({
      line: "    CHUNK-SIZE sem-model2vec: 1 chunks selected, avg=2 max=2",
      warning: null,
    });

    const oversized = "token ".repeat(2050);
    const formatted = formatChunkSizeLog("sem-model2vec", [oversized]);
    expect(formatted.line).not.toContain("over2048=0");
    expect(formatted.warning).toContain("WARNING");
    expect(formatted.warning).toContain(">2048 tokens");
  });

  it("builds paired legacy and budget semantic runs for context comparison", () => {
    expect(buildSemanticRuns(["model2vec"], "compare", {
      totalTokens: 4096,
      perChunkTokens: 384,
      softOverflowTokens: 128,
    })).toEqual([
      {
        key: "model2vec:legacy",
        backend: "model2vec",
        variant: "legacy",
        modeSuffix: "-legacy",
        retrievalIntelligenceV2: false,
        request: {},
      },
      {
        key: "model2vec:budget",
        backend: "model2vec",
        variant: "budget",
        modeSuffix: "-budget",
        retrievalIntelligenceV2: true,
        request: {
          context_budget_enabled: true,
          profile: "agent_fast",
          context_total_tokens: 4096,
          context_per_candidate_tokens: 384,
          context_soft_overflow_tokens: 128,
        },
      },
    ]);
  });

  it("caps legacy semantic snippets to the historical top-three context surface", () => {
    const results = Array.from({ length: 6 }, (_, i) => ({
      file: `src/${i}.rs`,
      snippet: `snippet ${i}`,
      start_line: i + 1,
      end_line: i + 2,
    }));

    const capped = applyLegacySnippetCap(results);

    expect(capped).toHaveLength(6);
    expect(capped.slice(0, 3).every((result) => result.snippet)).toBe(true);
    expect(capped.slice(3).every((result) => result.snippet === undefined)).toBe(true);
    expect(capped.slice(3).every((result) => result.start_line === undefined)).toBe(true);
  });

  it("builds a feature branch comparison table from suite aggregates", () => {
    const rows = buildFeatureBranchComparison({
      semantic_nl: [
        { mode: "aft-grep", recall: 0.1, mrr: 0.1, ndcg: 0.1, p50_ms: 20, p95_ms: 30, count: 10, empty: 0, snippets_per_query: 0, tokens_per_query: 0, max_doc_tokens: 0 },
        { mode: "semantic-fe-legacy", recall: 0.5, mrr: 0.4, ndcg: 0.45, p50_ms: 60, p95_ms: 80, count: 10, empty: 0, snippets_per_query: 3, tokens_per_query: 150, max_doc_tokens: 80 },
        { mode: "semantic-fe-budget", recall: 0.6, mrr: 0.45, ndcg: 0.5, p50_ms: 70, p95_ms: 100, count: 10, empty: 0, snippets_per_query: 10, tokens_per_query: 500, max_doc_tokens: 120 },
        { mode: "semantic-m2v-budget", recall: 0.7, mrr: 0.55, ndcg: 0.6, p50_ms: 50, p95_ms: 70, count: 10, empty: 0, snippets_per_query: 10, tokens_per_query: 420, max_doc_tokens: 100 },
        { mode: "hybrid-fe-legacy", recall: 0.85, mrr: 0.7, ndcg: 0.72, p50_ms: 80, p95_ms: 120, count: 10, empty: 0, snippets_per_query: 3, tokens_per_query: 200, max_doc_tokens: 80 },
        { mode: "hybrid-fe-budget", recall: 0.85, mrr: 0.7, ndcg: 0.72, p50_ms: 82, p95_ms: 125, count: 10, empty: 0, snippets_per_query: 8, tokens_per_query: 520, max_doc_tokens: 120 },
        { mode: "fts5", recall: 0.4, mrr: 0.3, ndcg: 0.35, p50_ms: 15, p95_ms: 25, count: 10, empty: 0, snippets_per_query: 1, tokens_per_query: 40, max_doc_tokens: 50 },
      ],
      identifier_exact: [
        { mode: "aft-grep", recall: 0.3, mrr: 0.2, ndcg: 0.25, p50_ms: 20, p95_ms: 40, count: 10, empty: 0, snippets_per_query: 0, tokens_per_query: 0, max_doc_tokens: 0 },
        { mode: "fts5_find_symbol_exact", recall: 0.8, mrr: 0.7, ndcg: 0.75, p50_ms: 8, p95_ms: 12, count: 10, empty: 0, snippets_per_query: 0, tokens_per_query: 0, max_doc_tokens: 0 },
      ],
    });

    expect(rows.map((row) => row.featureMode)).toContain("semantic-m2v-budget");
    expect(rows.map((row) => row.featureMode)).toContain("fts5_find_symbol_exact");
    expect(rows.find((row) => row.featureMode === "semantic-fe-budget")?.recallDelta).toBeCloseTo(0.1);
    expect(rows.find((row) => row.featureMode === "fts5_find_symbol_exact")?.recallDeltaPercentagePoints).toBeCloseTo(50);
    expect(rows.find((row) => row.featureMode === "hybrid-fe-budget")?.snippetDelta).toBeCloseTo(5);
  });
});
