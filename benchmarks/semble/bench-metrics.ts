/**
 * Benchmark metrics: attempt rows, scoring, aggregation, and latency decomposition.
 *
 * Every (suite, mode, query) triple emits exactly one attempt row.
 * Aggregates use attempted-row denominators, not only successful non-empty rows.
 */

import type { SearchResult } from "./bench-modes";

// ---------------------------------------------------------------------------
// Attempt row model
// ---------------------------------------------------------------------------

export type AttemptStatus = "ok" | "empty" | "error" | "unavailable" | "timeout";

export interface LatencyParts {
  configure_ms?: number;
  index_update_ms?: number;
  model_load_ms?: number;
  warmup_ms?: number;
  query_ms: number;
  rerank_ms?: number;
  end_to_end_ms: number;
}

export interface QueryAttempt {
  schema_version: number;
  suite: string;
  mode: string;
  query_id: string;
  query: string;
  repo_name: string;
  attempted: true;
  status: AttemptStatus;
  reason?: string;
  latency_ms: number;
  latency_parts: LatencyParts;
  results: SearchResult[];
  result_count: number;
  recall_at_k: number;
  mrr: number;
  ndcg_at_k: number;
  // Paired rerank metrics (only for rerank modes)
  rerank?: {
    base_mode: string;
    candidate_pool_size: number;
    rerank_pool_size: number;
    pre_rerank_recall: number;
    pre_rerank_mrr: number;
    pre_rerank_ndcg: number;
    post_rerank_recall: number;
    post_rerank_mrr: number;
    post_rerank_ndcg: number;
    rerank_delta_ndcg: number;
    rerank_latency_ms: number;
  };
}

// ---------------------------------------------------------------------------
// Scoring helpers (robust to string/object/undefined relevant entries)
// ---------------------------------------------------------------------------

function normalizePath(p: string): string {
  if (!p) return "";
  return p.replace(/\\/g, "/").replace(/^\/\?\//, "").replace(/^\.\//, "").toLowerCase();
}

function extractRelevantPaths(relevant: unknown[]): string[] {
  const paths: string[] = [];
  for (const r of relevant) {
    if (typeof r === "string") {
      paths.push(r);
    } else if (r && typeof r === "object" && "path" in r) {
      paths.push((r as { path: string }).path);
    }
  }
  return paths;
}

function pathMatches(a: string, b: string): boolean {
  const na = normalizePath(a);
  const nb = normalizePath(b);
  return Boolean(na && nb && (na === nb || na.endsWith(`/${nb}`) || nb.endsWith(`/${na}`)));
}

export function recallAtK(retrieved: SearchResult[], relevant: unknown[], k: number): number {
  if (!retrieved || relevant.length === 0) return 0;
  const paths = extractRelevantPaths(relevant);
  if (paths.length === 0) return 0;
  let hits = 0;
  for (const r of paths) {
    if (retrieved.slice(0, k).some((ret) => pathMatches(ret.file, r))) hits++;
  }
  return hits / paths.length;
}

export function mrr(retrieved: SearchResult[], relevant: unknown[]): number {
  if (!retrieved) return 0;
  const paths = extractRelevantPaths(relevant);
  if (paths.length === 0) return 0;
  for (let i = 0; i < retrieved.length; i++) {
    if (paths.some((r) => pathMatches(retrieved[i].file, r))) return 1 / (i + 1);
  }
  return 0;
}

export function ndcgAtK(retrieved: SearchResult[], relevant: unknown[], k: number): number {
  if (!retrieved) return 0;
  const paths = extractRelevantPaths(relevant);
  if (paths.length === 0) return 0;
  const relSet = new Set(paths.map(normalizePath));
  let dcg = 0;
  const matched = new Set<string>();
  for (let i = 0; i < Math.min(k, retrieved.length); i++) {
    const rf = normalizePath(retrieved[i].file);
    for (const r of relSet) {
      if (!matched.has(r) && (rf.endsWith(r) || r.endsWith(rf))) {
        matched.add(r);
        dcg += 1 / Math.log2(i + 2);
        break;
      }
    }
  }
  const idealHits = Math.min(relSet.size, k);
  let idcg = 0;
  for (let i = 0; i < idealHits; i++) idcg += 1 / Math.log2(i + 2);
  return idcg > 0 ? dcg / idcg : 0;
}

// ---------------------------------------------------------------------------
// Create attempt row
// ---------------------------------------------------------------------------

export function createAttempt(opts: {
  suite: string;
  mode: string;
  query_id: string;
  query: string;
  repo_name: string;
  status: AttemptStatus;
  reason?: string;
  results: SearchResult[];
  latency_ms: number;
  latency_parts?: Partial<LatencyParts>;
  relevant?: unknown[];
  k: number;
}): QueryAttempt {
  const relevant = opts.relevant ?? [];
  const isOk = opts.status === "ok";
  return {
    schema_version: 1,
    suite: opts.suite,
    mode: opts.mode,
    query_id: opts.query_id,
    query: opts.query,
    repo_name: opts.repo_name,
    attempted: true,
    status: opts.status,
    reason: opts.reason,
    latency_ms: opts.latency_ms,
    latency_parts: {
      query_ms: opts.latency_ms,
      end_to_end_ms: opts.latency_ms,
      ...opts.latency_parts,
    },
    results: opts.results,
    result_count: opts.results.length,
    recall_at_k: isOk ? recallAtK(opts.results, relevant, opts.k) : 0,
    mrr: isOk ? mrr(opts.results, relevant) : 0,
    ndcg_at_k: isOk ? ndcgAtK(opts.results, relevant, opts.k) : 0,
  };
}

// ---------------------------------------------------------------------------
// Aggregation (denominator = attempted, not only non-empty)
// ---------------------------------------------------------------------------

export interface AggregateResult {
  suite: string;
  mode: string;
  total_attempted: number;
  ok: number;
  empty: number;
  error: number;
  unavailable: number;
  timeout: number;
  recall: number;
  mrr: number;
  ndcg: number;
  p50_ms: number;
  p95_ms: number;
}

function percentile(sorted: number[], p: number): number {
  if (sorted.length === 0) return 0;
  const idx = Math.ceil(sorted.length * p / 100) - 1;
  return sorted[Math.max(0, idx)];
}

/**
 * Aggregate attempts by (suite, mode).
 *
 * The denominator is the count of ALL attempted rows (including empty/error/unavailable),
 * not just the count of rows with non-empty results.
 */
export function aggregateBySuiteMode(attempts: QueryAttempt[]): AggregateResult[] {
  const groups = new Map<string, QueryAttempt[]>();
  for (const a of attempts) {
    const key = `${a.suite}|${a.mode}`;
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key)!.push(a);
  }

  const results: AggregateResult[] = [];
  for (const [key, rows] of groups) {
    const [suite, mode] = key.split("|");
    const n = rows.length; // denominator = ALL attempted rows
    const latencies = rows.map((r) => r.latency_ms).sort((a, b) => a - b);

    const statusCounts = { ok: 0, empty: 0, error: 0, unavailable: 0, timeout: 0 };
    for (const r of rows) {
      statusCounts[r.status]++;
    }

    results.push({
      suite,
      mode,
      total_attempted: n,
      ok: statusCounts.ok,
      empty: statusCounts.empty,
      error: statusCounts.error,
      unavailable: statusCounts.unavailable,
      timeout: statusCounts.timeout,
      recall: n > 0 ? rows.reduce((s, r) => s + r.recall_at_k, 0) / n : 0,
      mrr: n > 0 ? rows.reduce((s, r) => s + r.mrr, 0) / n : 0,
      ndcg: n > 0 ? rows.reduce((s, r) => s + r.ndcg_at_k, 0) / n : 0,
      p50_ms: percentile(latencies, 50),
      p95_ms: percentile(latencies, 95),
    });
  }

  return results.sort((a, b) => a.suite.localeCompare(b.suite) || a.mode.localeCompare(b.mode));
}

// ---------------------------------------------------------------------------
// Rerank pairing
// ---------------------------------------------------------------------------

/**
 * Compute paired rerank metrics for a base attempt and a rerank attempt
 * on the same query.
 */
export function computeRerankPair(
  baseAttempt: QueryAttempt,
  rerankAttempt: QueryAttempt,
  candidatePoolSize: number,
  rerankPoolSize: number,
): QueryAttempt["rerank"] {
  return {
    base_mode: baseAttempt.mode,
    candidate_pool_size: candidatePoolSize,
    rerank_pool_size: rerankPoolSize,
    pre_rerank_recall: baseAttempt.recall_at_k,
    pre_rerank_mrr: baseAttempt.mrr,
    pre_rerank_ndcg: baseAttempt.ndcg_at_k,
    post_rerank_recall: rerankAttempt.recall_at_k,
    post_rerank_mrr: rerankAttempt.mrr,
    post_rerank_ndcg: rerankAttempt.ndcg_at_k,
    rerank_delta_ndcg: rerankAttempt.ndcg_at_k - baseAttempt.ndcg_at_k,
    rerank_latency_ms: rerankAttempt.latency_ms,
  };
}
