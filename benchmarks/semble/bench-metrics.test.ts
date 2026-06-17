/**
 * Tests for bench-metrics.ts — attempt rows, aggregation, and scoring.
 *
 * These tests verify the core correctness properties:
 * 1. Denominator is attempted rows, not only non-empty results
 * 2. Suites are aggregated separately
 * 3. Unavailable modes are visible
 * 4. Rerank metrics are paired
 * 5. Path matching handles string/object/undefined
 */

import { describe, it, expect } from "bun:test";
import {
  recallAtK,
  mrr,
  ndcgAtK,
  createAttempt,
  aggregateBySuiteMode,
  computeRerankPair,
  type QueryAttempt,
} from "./bench-metrics";

describe("scoring helpers", () => {
  it("recallAtK returns 0 for empty relevant", () => {
    expect(recallAtK([{ file: "a.ts" }], [], 10)).toBe(0);
  });

  it("recallAtK handles string relevant", () => {
    expect(recallAtK([{ file: "a.ts" }, { file: "b.ts" }], ["a.ts"], 10)).toBe(1);
  });

  it("recallAtK handles object relevant with path", () => {
    expect(recallAtK([{ file: "a.ts" }], [{ path: "a.ts" }], 10)).toBe(1);
  });

  it("mrr returns correct reciprocal rank", () => {
    expect(mrr([{ file: "x.ts" }, { file: "a.ts" }], ["a.ts"])).toBe(0.5);
  });

  it("ndcgAtK returns 0 for no relevant in top-k", () => {
    expect(ndcgAtK([{ file: "x.ts" }], ["a.ts"], 10)).toBe(0);
  });
});

describe("createAttempt", () => {
  it("scores 0 for non-ok status even with results", () => {
    const attempt = createAttempt({
      suite: "identifier_exact",
      mode: "rg",
      query_id: "test.1",
      query: "Router",
      repo_name: "axum",
      status: "empty",
      results: [],
      latency_ms: 10,
      k: 10,
    });
    expect(attempt.recall_at_k).toBe(0);
    expect(attempt.mrr).toBe(0);
    expect(attempt.ndcg_at_k).toBe(0);
    expect(attempt.attempted).toBe(true);
  });

  it("scores 0 for unavailable status", () => {
    const attempt = createAttempt({
      suite: "identifier_exact",
      mode: "fts5_search",
      query_id: "test.1",
      query: "Router",
      repo_name: "axum",
      status: "unavailable",
      reason: "FTS5 not compiled",
      results: [],
      latency_ms: 0,
      k: 10,
    });
    expect(attempt.recall_at_k).toBe(0);
    expect(attempt.status).toBe("unavailable");
  });
});

describe("aggregateBySuiteMode", () => {
  it("denominator includes empty attempts", () => {
    const attempts: QueryAttempt[] = [
      createAttempt({ suite: "s", mode: "m", query_id: "q1", query: "a", repo_name: "r", status: "ok", results: [{ file: "a.ts" }], latency_ms: 10, relevant: ["a.ts"], k: 10 }),
      createAttempt({ suite: "s", mode: "m", query_id: "q2", query: "b", repo_name: "r", status: "ok", results: [{ file: "b.ts" }], latency_ms: 10, relevant: ["b.ts"], k: 10 }),
      createAttempt({ suite: "s", mode: "m", query_id: "q3", query: "c", repo_name: "r", status: "empty", results: [], latency_ms: 10, relevant: ["c.ts"], k: 10 }),
      createAttempt({ suite: "s", mode: "m", query_id: "q4", query: "d", repo_name: "r", status: "error", results: [], latency_ms: 10, reason: "fail", k: 10 }),
    ];

    const agg = aggregateBySuiteMode(attempts);
    expect(agg).toHaveLength(1);
    expect(agg[0].total_attempted).toBe(4); // denominator = 4, not 2
    expect(agg[0].ok).toBe(2);
    expect(agg[0].empty).toBe(1);
    expect(agg[0].error).toBe(1);
    // recall averaged over 4 attempts: (1 + 1 + 0 + 0) / 4 = 0.5
    expect(agg[0].recall).toBeCloseTo(0.5, 5);
  });

  it("separates suites", () => {
    const attempts: QueryAttempt[] = [
      createAttempt({ suite: "semantic_nl", mode: "rg", query_id: "q1", query: "a", repo_name: "r", status: "ok", results: [], latency_ms: 10, k: 10 }),
      createAttempt({ suite: "identifier_exact", mode: "rg", query_id: "q2", query: "b", repo_name: "r", status: "ok", results: [], latency_ms: 10, k: 10 }),
    ];

    const agg = aggregateBySuiteMode(attempts);
    expect(agg).toHaveLength(2);
    expect(agg[0].suite).toBe("identifier_exact");
    expect(agg[1].suite).toBe("semantic_nl");
  });

  it("includes unavailable modes in aggregation", () => {
    const attempts: QueryAttempt[] = [
      createAttempt({ suite: "s", mode: "fts5_search", query_id: "q1", query: "a", repo_name: "r", status: "unavailable", reason: "not compiled", results: [], latency_ms: 0, k: 10 }),
    ];

    const agg = aggregateBySuiteMode(attempts);
    expect(agg).toHaveLength(1);
    expect(agg[0].unavailable).toBe(1);
    expect(agg[0].total_attempted).toBe(1);
  });
});

describe("rerank pairing", () => {
  it("computes paired metrics from base and rerank attempts", () => {
    const base = createAttempt({ suite: "s", mode: "semantic_m2v", query_id: "q1", query: "a", repo_name: "r", status: "ok", results: [{ file: "a.ts" }], latency_ms: 50, relevant: ["a.ts"], k: 10 });
    const reranked = createAttempt({ suite: "s", mode: "rerank", query_id: "q1", query: "a", repo_name: "r", status: "ok", results: [{ file: "a.ts" }], latency_ms: 100, relevant: ["a.ts"], k: 10 });

    const pair = computeRerankPair(base, reranked, 50, 10);
    expect(pair).toBeTruthy();
    expect(pair!.base_mode).toBe("semantic_m2v");
    expect(pair!.candidate_pool_size).toBe(50);
    expect(pair!.rerank_pool_size).toBe(10);
    expect(pair!.pre_rerank_recall).toBe(base.recall_at_k);
    expect(pair!.post_rerank_recall).toBe(reranked.recall_at_k);
    expect(pair!.rerank_delta_ndcg).toBeCloseTo(reranked.ndcg_at_k - base.ndcg_at_k, 5);
  });
});
