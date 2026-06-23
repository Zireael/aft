#!/usr/bin/env bun
/**
 * Comprehensive AFT Search Benchmark
 *
 * Tests lexical, semantic, hybrid, and reranked search across multiple providers.
 *
 * Usage:
 *   bun run benchmarks/semble/pilot.ts --binary ./target/release/aft/aft.exe --k 10
 *   bun run benchmarks/semble/pilot.ts --binary <path> --rerank --semantic-api-url http://localhost:8090/v1
 *
 * Options:
 *   --binary <path>          AFT binary path
 *   --k <n>                  Top-k (default: 10)
 *   --cache-dir <dir>        Repo cache directory (default: .bench-cache)
 *   --backend <list>         Semantic backends: both,model2vec,fastembed,semantic-api,skip (default: both)
 *   --semantic-api-url <u>   OpenAI-compatible endpoint URL for semantic-api
 *   --semantic-api-model <m> Model name for semantic-api (auto-detect if omitted)
 *   --rerank                 Enable reranker pass (5x oversampling)
 *   --rerank-model <name>    Reranker model (default: GTE-Reranker-Modernbert)
 *   --rerank-url <url>       Reranker endpoint (default: http://127.0.0.1:8090/v1/rerank)
 *   --include-lexical        Include lexical identifier queries (default: true)
 *   --identifier-semantic    Include semantic backends in identifier suites (default: false)
 *   --output <file>          JSON report output path
 *   --verbose, -v            Per-query debug output
 */

import { readFileSync, writeFileSync, existsSync, statSync, mkdirSync } from "fs";
import { join, resolve } from "path";
import { execSync } from "child_process";
import { AftSession } from "./aft-ndjson";
import { runPreflight, printPreflight } from "./bench-cli";
import { loadCanonSuite, loadCanonRepos } from "./canon-loader";
import { discoverModels, verifySpecificModels, ensureModelLoaded, formatDiscoveredModels, interactiveModelSelection, type ModelDiscoveryResult } from "./model-discovery";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface SearchResult {
  file: string;
  line?: number;
  score?: number;
  content?: string;
  snippet?: string;
  start_line?: number;
  end_line?: number;
  symbol_id?: number;
}

interface ContextQuality {
  candidate_pool_size: number;
  rerank_pool_size: number;
  snippet_count: number;
  enriched_candidate_count: number;
  path_only_count: number;
  unenriched_candidate_count: number;
  avg_doc_tokens: number;
  max_doc_tokens: number;
  total_doc_tokens: number;
  pre_rerank_recall_at_pool: number;
  post_rerank_recall_at_k: number;
  lost_relevant_after_rerank: string[];
  context_exhausted: boolean | null;
  reranker_skipped_reason: string | null;
  intent_distribution: Record<string, number>;
  tuning_recall_at_10: number;
  holdout_recall_at_10: number;
  engine_latency_p95_ms: number;
  benchmark_harness_latency_p95_ms: number;
}

export interface ModeResult {
  mode: string;
  query: string;
  repo_name: string;
  category: string;
  suite: string;
  latency_ms: number;
  results: SearchResult[];
  recall_at_k: number;
  mrr: number;
  ndcg_at_k: number;
  status?: "ok" | "empty" | "unavailable" | "error";
  reason?: string;
}

// ---------------------------------------------------------------------------
// Approximate token counting (~4 chars/token for code)
// ---------------------------------------------------------------------------

function approxTokens(text: string): number {
  // Simple heuristic: split on whitespace/punctuation, ~1 token per word
  // More accurate than chars/4 for code with lots of short identifiers
  if (!text) return 0;
  let count = 0;
  for (let i = 0; i < text.length; ) {
    // Skip whitespace
    while (i < text.length && /\s/.test(text[i])) i++;
    if (i >= text.length) break;
    count++;
    // Skip non-whitespace
    while (i < text.length && /\S/.test(text[i])) i++;
  }
  return count;
}

export function formatChunkSizeLog(label: string, chunks: string[]): { line: string; warning: string | null } {
  const sizes = chunks.map(approxTokens);
  const over = sizes.filter((s) => s > 2048).length;
  const max = Math.max(...sizes);
  const avg = Math.round(sizes.reduce((a, b) => a + b, 0) / sizes.length);
  return {
    line: `    CHUNK-SIZE ${label}: ${chunks.length} chunks, avg=${avg} max=${max}`,
    warning: over > 0 ? `    \x1b[0;33mWARNING ${label}: ${over} chunk(s) >2048 tokens; max=${max}\x1b[0m` : null,
  };
}

function logChunkSizes(label: string, chunks: string[], verbose: boolean): void {
  if (!verbose || chunks.length === 0) return;
  const formatted = formatChunkSizeLog(label, chunks);
  console.log(formatted.line);
  if (formatted.warning) console.log(formatted.warning);
}

interface AggregateMode {
  mode: string;
  recall: number;
  mrr: number;
  ndcg: number;
  p50_ms: number;
  p95_ms: number;
  count: number;
  empty: number;
  snippets_per_query: number;
  tokens_per_query: number;
  max_doc_tokens: number;
}

interface IntentMetric {
  recall_at_10: number;
  mrr: number;
  ndcg_at_10: number;
  count: number;
  tuning_recall_at_10: number;
  holdout_recall_at_10: number;
}

interface RerankMetrics {
  pre_rerank_recall: number;
  post_rerank_recall: number;
  post_rerank_mrr: number;
  post_rerank_ndcg: number;
  rerank_delta_ndcg: number;
  rerank_p50_ms: number;
  rerank_p95_ms: number;
}

interface ContextQuality {
  candidate_pool_size: number;
  rerank_pool_size: number;
  snippet_count: number;
  enriched_candidate_count: number;
  path_only_count: number;
  unenriched_candidate_count: number;
  avg_doc_tokens: number;
  max_doc_tokens: number;
  total_doc_tokens: number;
  pre_rerank_recall_at_pool: number;
  post_rerank_recall_at_k: number;
  lost_relevant_after_rerank: string[];
  context_exhausted: boolean | null;
  reranker_skipped_reason: string | null;
  intent_distribution: Record<string, number>;
  tuning_recall_at_10: number;
  holdout_recall_at_10: number;
  engine_latency_p95_ms: number;
  benchmark_harness_latency_p95_ms: number;
}

// ---------------------------------------------------------------------------
// Scoring helpers
// ---------------------------------------------------------------------------

function normalizePath(p: string): string {
  if (!p) return "";
  return p.replace(/\\/g, "/").replace(/^\/\?\//, "").replace(/^\.\//, "").toLowerCase();
}

function normalizeRepoArg(repo: string): string {
  const trimmed = repo.trim().replace(/\/+$/, "");
  if (!trimmed) return trimmed;
  const parts = trimmed.split("/");
  return parts[parts.length - 1] || trimmed;
}

function canonicalIntent(category: string | undefined): string {
  switch ((category || "").toLowerCase()) {
    case "architecture":
    case "semantic":
    case "natural_language":
    case "naturallanguage":
      return "NaturalLanguage";
    case "symbol":
    case "identifier":
    case "identifier_exact":
    case "identifier_prefix":
    case "exact_symbol":
    case "exactsymbol":
      return "ExactSymbol";
    case "path":
    case "path_lookup":
    case "pathlookup":
      return "PathLookup";
    case "diagnostic":
    case "diagnostic_error":
    case "diagnosticerror":
      return "DiagnosticError";
    default:
      return category || "unknown";
  }
}

function pathMatches(a: string, b: string): boolean {
  const na = normalizePath(a);
  const nb = normalizePath(b);
  return Boolean(na && nb && (na === nb || na.endsWith(`/${nb}`) || nb.endsWith(`/${na}`)));
}

function recallAtK(retrieved: SearchResult[], relevant: string[], k: number): number {
  if (!retrieved || relevant.length === 0) return 0;
  let hits = 0;
  for (const r of relevant) {
    if (retrieved.slice(0, k).some((ret) => pathMatches(ret.file, r))) hits++;
  }
  return hits / relevant.length;
}

function mrr(retrieved: SearchResult[], relevant: string[]): number {
  if (!retrieved) return 0;
  for (let i = 0; i < retrieved.length; i++) {
    if (relevant.some((r) => pathMatches(retrieved[i].file, r))) return 1 / (i + 1);
  }
  return 0;
}

export function ndcgAtK(retrieved: SearchResult[], relevant: string[], k: number): number {
  if (!retrieved) return 0;
  const relSet = new Set(relevant.map(normalizePath));
  let dcg = 0;
  const matched = new Set<string>();
  for (let i = 0; i < Math.min(k, retrieved.length); i++) {
    const rf = normalizePath(retrieved[i].file);
    if (!rf) continue;
    for (const r of relSet) {
      if (!r) continue;
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

function percentile(sorted: number[], p: number): number {
  if (sorted.length === 0) return 0;
  const idx = Math.ceil(sorted.length * p / 100) - 1;
  return sorted[Math.max(0, idx)];
}

export function aggregateMetrics(rows: ModeResult[], totalQueries: number): AggregateMode {
  const mode = rows.length > 0 ? rows[0].mode : "unknown";
  const n = rows.length;
  const latencies = rows.map((r) => r.latency_ms).sort((a, b) => a - b);
  const snippetCounts = rows.map((row) => row.results.filter((result) => (result.snippet || "").length > 0).length);
  const tokenCounts = rows.map((row) => row.results.reduce((sum, result) => sum + approxTokens(result.snippet || ""), 0));
  const maxDocTokens = rows.flatMap((row) => row.results.map((result) => approxTokens(result.snippet || "")));
  return {
    mode,
    recall: n > 0 ? rows.reduce((s, r) => s + r.recall_at_k, 0) / n : 0,
    mrr: n > 0 ? rows.reduce((s, r) => s + r.mrr, 0) / n : 0,
    ndcg: n > 0 ? rows.reduce((s, r) => s + r.ndcg_at_k, 0) / n : 0,
    p50_ms: percentile(latencies, 50),
    p95_ms: percentile(latencies, 95),
    count: n,
    empty: Math.max(0, totalQueries - n),
    snippets_per_query: n > 0 ? rows.reduce((s, _row, i) => s + snippetCounts[i], 0) / n : 0,
    tokens_per_query: n > 0 ? rows.reduce((s, _row, i) => s + tokenCounts[i], 0) / n : 0,
    max_doc_tokens: maxDocTokens.length > 0 ? Math.max(...maxDocTokens) : 0,
  };
}

export function splitAggregatesBySuite(
  rows: ModeResult[],
  suiteTotals: Record<string, number>,
): { semantic: AggregateMode[]; lexical: AggregateMode[]; bySuite: Record<string, AggregateMode[]> } {
  const bySuiteMode = new Map<string, ModeResult[]>();
  for (const row of rows) {
    const suite = row.suite || row.category || "semantic_nl";
    const key = `${suite}\0${row.mode}`;
    if (!bySuiteMode.has(key)) bySuiteMode.set(key, []);
    bySuiteMode.get(key)!.push(row);
  }

  const bySuite: Record<string, AggregateMode[]> = {};
  for (const [key, suiteRows] of bySuiteMode) {
    const [suite] = key.split("\0");
    if (!bySuite[suite]) bySuite[suite] = [];
    bySuite[suite].push(aggregateMetrics(suiteRows, suiteTotals[suite] ?? suiteRows.length));
  }

  for (const metrics of Object.values(bySuite)) {
    metrics.sort((a, b) => b.recall - a.recall);
  }

  return {
    semantic: bySuite.semantic_nl ?? [],
    lexical: Object.entries(bySuite)
      .filter(([suite]) => suite !== "semantic_nl")
      .flatMap(([, metrics]) => metrics)
      .sort((a, b) => b.recall - a.recall),
    bySuite,
  };
}

export interface LexicalBenchmarkQuery {
  id: string;
  query: string;
  repos: string[];
  repo_name: string;
  category: string;
  suite: "identifier_exact" | "identifier_prefix";
  relevant: string[];
  secondary: string[];
}

export function buildLexicalQueriesFromCanon(canonDir: string, repoFilters = new Set<string>()): LexicalBenchmarkQuery[] {
  const suites = [
    ["identifier_exact", loadCanonSuite(resolve(canonDir), "identifier_exact")],
    ["identifier_prefix", loadCanonSuite(resolve(canonDir), "identifier_prefix")],
  ] as const;
  const queries: LexicalBenchmarkQuery[] = [];

  for (const [suite, canon] of suites) {
    for (const q of canon?.queries || []) {
      if (repoFilters.size > 0 && !repoFilters.has(q.repo_name)) continue;
      const relevant = (q.relevant || []).map((r: any) => r.path).filter(Boolean);
      const secondary = (q.secondary || []).map((r: any) => r.path).filter(Boolean);
      if (relevant.length + secondary.length === 0) continue;
      queries.push({
        id: q.id || `${q.repo_name}.${suite}.${q.query}`,
        query: q.query,
        repos: [q.repo_name],
        repo_name: q.repo_name,
        category: suite,
        suite,
        relevant,
        secondary,
      });
    }
  }

  return queries;
}

export function groupLexicalQueriesByRepo(
  queries: LexicalBenchmarkQuery[],
): Array<[string, LexicalBenchmarkQuery[]]> {
  const byRepo = new Map<string, LexicalBenchmarkQuery[]>();
  for (const query of queries) {
    for (const repo of query.repos) {
      if (!byRepo.has(repo)) byRepo.set(repo, []);
      byRepo.get(repo)!.push(query);
    }
  }

  return [...byRepo.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([repo, repoQueries]) => [
      repo,
      [...repoQueries].sort((a, b) => {
        const suiteOrder = a.suite.localeCompare(b.suite);
        return suiteOrder !== 0 ? suiteOrder : a.id.localeCompare(b.id);
      }),
    ]);
}

export function shouldRunIdentifierSemantic(_profileName: string, explicit: boolean | undefined): boolean {
  return explicit === true;
}

export function identifierModePlan(
  suite: "identifier_exact" | "identifier_prefix",
  backends: string[],
  includeSemantic: boolean,
): string[] {
  const modes = [
    "lexical (rg)",
    "aft-grep",
    "fts5",
    suite === "identifier_exact" ? "fts5_find_symbol_exact" : "fts5_find_symbol_prefix",
  ];
  if (includeSemantic) {
    modes.push(...backends.map((be) => (
      be === "model2vec" ? "semantic-m2v" : be === "fastembed" ? "semantic-fe" : "semantic-api"
    )));
  }
  return modes;
}

export type ContextBenchmarkMode = "legacy" | "budget" | "compare";

export interface ContextBudgetOptions {
  totalTokens?: number;
  perChunkTokens?: number;
  softOverflowTokens?: number;
}

export interface SemanticRun {
  key: string;
  backend: string;
  variant: "legacy" | "budget";
  modeSuffix: string;
  retrievalIntelligenceV2: boolean;
  request: Record<string, unknown>;
}

export function buildSemanticRuns(
  backends: string[],
  contextMode: ContextBenchmarkMode,
  budget: ContextBudgetOptions,
): SemanticRun[] {
  const variants: Array<"legacy" | "budget"> = contextMode === "compare"
    ? ["legacy", "budget"]
    : [contextMode];
  const includeSuffix = contextMode === "compare" || contextMode === "budget";

  return backends.flatMap((backend) => variants.map((variant) => {
    const request: Record<string, unknown> = {};
    if (variant === "budget") {
      request.context_budget_enabled = true;
      request.profile = "agent_fast";
      if (budget.totalTokens !== undefined) request.context_total_tokens = budget.totalTokens;
      if (budget.perChunkTokens !== undefined) request.context_per_candidate_tokens = budget.perChunkTokens;
      if (budget.softOverflowTokens !== undefined) request.context_soft_overflow_tokens = budget.softOverflowTokens;
    }

    return {
      key: `${backend}:${variant}`,
      backend,
      variant,
      modeSuffix: includeSuffix ? `-${variant}` : "",
      retrievalIntelligenceV2: variant === "budget",
      request,
    };
  }));
}

function semanticModeName(run: SemanticRun): string {
  const base = run.backend === "model2vec" ? "semantic-m2v" : run.backend === "fastembed" ? "semantic-fe" : "semantic-api";
  return `${base}${run.modeSuffix}`;
}

export function buildEmptyCounts(rows: ModeResult[]): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const row of rows) {
    if (row.status !== "empty") continue;
    const key = `${row.suite || row.category || "unknown"}/${row.mode}`;
    counts[key] = (counts[key] || 0) + 1;
  }
  return Object.fromEntries(Object.entries(counts).sort(([a], [b]) => a.localeCompare(b)));
}

function scoredRow(opts: {
  mode: string;
  query: string;
  repo_name: string;
  category: string;
  suite: string;
  latency_ms: number;
  results: SearchResult[];
  relevant: string[];
  k: number;
  status?: ModeResult["status"];
  reason?: string;
}): ModeResult {
  const status = opts.status ?? (opts.results.length > 0 ? "ok" : "empty");
  const scoreable = status === "ok";
  return {
    mode: opts.mode,
    query: opts.query,
    repo_name: opts.repo_name,
    category: opts.category,
    suite: opts.suite,
    latency_ms: opts.latency_ms,
    results: opts.results,
    recall_at_k: scoreable ? recallAtK(opts.results, opts.relevant, opts.k) : 0,
    mrr: scoreable ? mrr(opts.results, opts.relevant) : 0,
    ndcg_at_k: scoreable ? ndcgAtK(opts.results, opts.relevant, opts.k) : 0,
    status,
    reason: opts.reason,
  };
}

function aggregateIntentMetrics(
  rows: ModeResult[],
  queryHoldOut: Map<string, boolean>,
): Record<string, IntentMetric> {
  const byIntent = new Map<string, ModeResult[]>();
  for (const row of rows) {
    const intent = canonicalIntent(row.category);
    if (!byIntent.has(intent)) byIntent.set(intent, []);
    byIntent.get(intent)!.push(row);
  }

  const out: Record<string, IntentMetric> = {};
  for (const [intent, intentRows] of byIntent) {
    const n = intentRows.length;
    const tuningRows = intentRows.filter((row) => queryHoldOut.get(row.query) !== true);
    const holdoutRows = intentRows.filter((row) => queryHoldOut.get(row.query) === true);
    out[intent] = {
      recall_at_10: n > 0 ? intentRows.reduce((s, r) => s + r.recall_at_k, 0) / n : 0,
      mrr: n > 0 ? intentRows.reduce((s, r) => s + r.mrr, 0) / n : 0,
      ndcg_at_10: n > 0 ? intentRows.reduce((s, r) => s + r.ndcg_at_k, 0) / n : 0,
      count: n,
      tuning_recall_at_10: tuningRows.length > 0
        ? tuningRows.reduce((s, r) => s + r.recall_at_k, 0) / tuningRows.length
        : 0,
      holdout_recall_at_10: holdoutRows.length > 0
        ? holdoutRows.reduce((s, r) => s + r.recall_at_k, 0) / holdoutRows.length
        : 0,
    };
  }
  return out;
}

// ---------------------------------------------------------------------------
// External reranking
// ---------------------------------------------------------------------------

let RERANK_MODEL = "GTE-Reranker-Modernbert";
let RERANK_URL = "http://127.0.0.1:8090/v1/rerank";
let RERANK_INSTRUCTION = "";

async function applyRerank(
  query: string,
  results: SearchResult[],
  k: number,
  repoDir: string,
  verbose: boolean,
  oversample: number = 10,
  rerankContext: string = "aft_output",
): Promise<{ results: SearchResult[]; latency_ms: number; reranker_skipped_reason?: string; enriched_candidate_count: number; path_only_count: number; snippet_count: number; avg_doc_tokens: number; max_doc_tokens: number; total_doc_tokens: number }> {
  const candidates = results.slice(0, k * oversample);
  if (candidates.length <= 1) return { results: candidates, latency_ms: 0, enriched_candidate_count: 0, path_only_count: 0, snippet_count: 0, avg_doc_tokens: 0, max_doc_tokens: 0, total_doc_tokens: 0 };

  const readStart = performance.now();
  // Use snippet from search results — these are logical code blocks from AFT's symbol resolution
  // For candidates without snippets (ranks 3+), read the symbol's line range from disk
  // UNLESS rerankContext is "aft_output" (zero file reads) or "path_only" (strip content)
  let snippetCount = 0;
  let lineRangeCount = 0;
  let pathCount = 0;
  const documents = candidates.map((r) => {
    // path_only mode: strip all snippet content, use only file path
    if (rerankContext === "path_only") {
      pathCount++;
      return r.file || "";
    }

    // Prefer snippet from semantic search results (already extracted by AFT as logical blocks)
    if (r.snippet && r.snippet.length > 10) {
      snippetCount++;
      return r.snippet;
    }

    // aft_output mode: zero source file reads — use file path as label
    if (rerankContext === "aft_output") {
      pathCount++;
      const rawFile = r.file || "";
      return rawFile.replace(/^\\\\\?\\/, "");
    }

    // benchmark_enriched mode: read from disk as fallback
    const startLine = r.start_line || r.line;
    const endLine = r.end_line;
    if (startLine && endLine && endLine > startLine) {
      const rawFile = r.file || "";
      const normalized = rawFile.replace(/^\\\\\?\\/, "");
      const maybeAbsolute = /^[A-Za-z]:[\\/]/.test(normalized) || normalized.startsWith("/");
      const resolved = maybeAbsolute ? normalized : join(repoDir, normalized);
      try {
        const content = readFileSync(resolved, "utf-8");
        const lines = content.split("\n");
        // Read the symbol's exact range (capped to 100 lines for safety)
        const cappedEnd = Math.min(endLine, startLine + 100);
        const span = lines.slice(Math.max(0, startLine - 1), cappedEnd).join("\n");
        if (span.length > 10) {
          lineRangeCount++;
          return span;
        }
      } catch { /* fall through */ }
    }

    // Last resort: file path as label
    pathCount++;
    const rawFile = r.file || "";
    const normalized = rawFile.replace(/^\\\\\?\\/, "");
    return normalized;
  });
  const readMs = performance.now() - readStart;
  // Compute document token stats for context quality tracking
  const docTokenCounts = documents.map(approxTokens);
  const enrichedCount = snippetCount + lineRangeCount;
  const docTokenStats = {
    enriched_candidate_count: enrichedCount,
    path_only_count: pathCount,
    snippet_count: snippetCount,
    avg_doc_tokens: documents.length > 0 ? Math.round(docTokenCounts.reduce((a, b) => a + b, 0) / documents.length) : 0,
    max_doc_tokens: documents.length > 0 ? Math.max(...docTokenCounts) : 0,
    total_doc_tokens: docTokenCounts.reduce((a, b) => a + b, 0),
  };
  // Log document source breakdown
  if (verbose && candidates.length > 0) {
    console.log(`    RERANK DOCS: ${snippetCount} snippets, ${lineRangeCount} line-ranges, ${pathCount} paths (${candidates.length} total)`);
  }
  // Log chunk sizes for token budget analysis
  logChunkSizes("reranker", documents, verbose);

  const start = performance.now();
  try {
    const resp = await fetch(RERANK_URL, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        model: RERANK_MODEL,
        query,
        documents,
        top_n: Math.min(k, candidates.length),
        ...(RERANK_INSTRUCTION ? { instruct: RERANK_INSTRUCTION } : {}),
      }),
      signal: AbortSignal.timeout(30_000),
    });

    if (!resp.ok) {
      if (verbose) console.log(`    RERANK HTTP ${resp.status}`);
      return { results: candidates, latency_ms: performance.now() - start, ...docTokenStats };
    }

    const json = await resp.json() as any;
    const items = json.results || json.data || [];
    const ranked: SearchResult[] = [];
    for (const item of items) {
      const idx = item.index ?? (item.document as any)?.index;
      if (typeof idx === "number" && idx >= 0 && idx < candidates.length) {
        ranked.push({ ...candidates[idx], score: item.relevance_score ?? item.score ?? candidates[idx].score });
      }
    }
    if (ranked.length === 0) return { results: candidates, latency_ms: readMs + (performance.now() - start), ...docTokenStats };

    const rankedKeys = new Set(ranked.map((r) => normalizePath(r.file)));
    const tail = candidates.filter((r) => !rankedKeys.has(normalizePath(r.file)));
    return { results: [...ranked, ...tail], latency_ms: readMs + (performance.now() - start), ...docTokenStats };
  } catch (e) {
    if (verbose) console.log(`    RERANK ERROR: ${e}`);
    return { results: candidates, latency_ms: readMs + (performance.now() - start), ...docTokenStats };
  }
}

// ---------------------------------------------------------------------------
// AFT grep (trigram) mode — persistent session
// ---------------------------------------------------------------------------

async function initGrepSession(bin: string, targetDir: string, verbose: boolean): Promise<AftSession | null> {
  const session = new AftSession(bin);
  try {
    await session.call({ command: "configure", harness: "opencode", project_root: targetDir, storage_dir: join(targetDir, ".aft-bench-grep") }, 30_000);
    return session;
  } catch (e) {
    if (verbose) console.log(`    GREP init ERROR: ${e}`);
    session.close();
    return null;
  }
}

async function grepQuery(session: AftSession, query: string, k: number, verbose: boolean): Promise<SearchResult[]> {
  try {
    const resp = await session.call({ command: "grep", pattern: query, max_results: k }, 30_000);
    const items = (resp as any).results || (resp as any).matches;
    if (items && Array.isArray(items)) {
      return items.map((r: any) => ({ file: r.file || r.file_path || r.path || "", line: r.start_line || r.line, score: r.score }));
    }
  } catch (e) { if (verbose) console.log(`    GREP ERROR: ${e}`); }
  return [];
}

// ---------------------------------------------------------------------------
// FTS5 mode — persistent session
// ---------------------------------------------------------------------------

async function initFts5Session(bin: string, targetDir: string, verbose: boolean): Promise<AftSession | null> {
  const session = new AftSession(bin);
  try {
    await session.call({ command: "configure", harness: "opencode", project_root: targetDir, storage_dir: join(targetDir, ".aft-bench-fts5"), fts5: { enabled: true } }, 30_000);
    await session.call({ command: "fts5_index", action: "update" }, 60_000);
    return session;
  } catch (e) {
    if (verbose) console.log(`    FTS5 init ERROR: ${e}`);
    session.close();
    return null;
  }
}

async function fts5Query(session: AftSession, query: string, k: number, verbose: boolean): Promise<SearchResult[]> {
  try {
    const resp = await session.call({ command: "fts5_search", query, scope: "all", top_k: k }, 30_000);
    const items = (resp as any).evidence || (resp as any).results || (resp as any).matches;
    if (items && Array.isArray(items)) {
      return items.map((r: any) => ({ file: r.file_path || r.path || r.file || "", line: r.start_line || r.line, score: r.score }));
    }
  } catch (e) { if (verbose) console.log(`    FTS5 ERROR: ${e}`); }
  return [];
}

export function symbolResultFromFts5Row(row: any, fallbackFile = ""): SearchResult {
  return {
    file: row.file_path || row.path || row.file || fallbackFile || "",
    line: row.start_line || row.line,
    score: row.score,
    symbol_id: typeof row.symbol_id === "number" ? row.symbol_id : undefined,
  };
}

async function resolveFts5SymbolFile(session: AftSession, row: any, verbose: boolean): Promise<string> {
  const symbolId = typeof row.symbol_id === "number" ? row.symbol_id : undefined;
  if (!symbolId) return "";
  try {
    const resp = await session.call({ command: "fts5_read_symbol", symbol_id: symbolId, context_lines: 0 }, 30_000);
    return String((resp as any).file_path || "");
  } catch (e) {
    if (verbose) console.log(`    FTS5 read-symbol fallback ERROR: ${e}`);
    return "";
  }
}

async function fts5FindSymbolQuery(
  session: AftSession,
  name: string,
  mode: "exact" | "prefix",
  k: number,
  verbose: boolean,
): Promise<SearchResult[]> {
  try {
    const resp = await session.call({ command: "fts5_find_symbol", name, mode, top_k: k }, 30_000);
    const items = (resp as any).symbols || (resp as any).results || (resp as any).evidence || (resp as any).matches;
    if (items && Array.isArray(items)) {
      const results: SearchResult[] = [];
      for (const r of items) {
        const initial = symbolResultFromFts5Row(r);
        if (!initial.file) {
          initial.file = await resolveFts5SymbolFile(session, r, verbose);
        }
        results.push(initial);
      }
      return results;
    }
  } catch (e) { if (verbose) console.log(`    FTS5 find-symbol ${mode} ERROR: ${e}`); }
  return [];
}

// ---------------------------------------------------------------------------
// Semantic mode — persistent session per repo
// ---------------------------------------------------------------------------

async function initSemanticSession(
  bin: string, targetDir: string, model: string, backend: string,
  verbose: boolean, storageDir?: string, queryPromptOverride?: string,
  retrievalIntelligenceV2?: boolean,
): Promise<AftSession | null> {
  const session = new AftSession(bin);
  try {
    const config: Record<string, unknown> = {
      command: "configure", harness: "opencode",
      project_root: targetDir,
      storage_dir: storageDir || join(targetDir, ".aft-bench"),
      semantic_search: true,
    };
    if (retrievalIntelligenceV2 !== undefined) {
      config.intelligence = { retrieval_intelligence_v2: retrievalIntelligenceV2 };
    }
    if (backend === "semantic-api") {
      const url = (globalThis as any).__SEMANTIC_API_URL || "";
      const modelName = (globalThis as any).__SEMANTIC_API_MODEL || "";
      const semantic: Record<string, unknown> = { backend: "openai_compatible", base_url: url, model: modelName };
      // CodeRankEmbed requires query prefix for optimal retrieval quality
      if (queryPromptOverride) {
        semantic.query_prompt_template = queryPromptOverride;
      } else if (modelName.toLowerCase().includes("coderankembed")) {
        semantic.query_prompt_template = "Represent this query for searching relevant code: {query}";
      }
      config.semantic = semantic;
    } else {
      config.semantic = { backend, model };
    }
    await session.call(config, 30_000);

    const deadline = Date.now() + 180_000;
    while (Date.now() < deadline) {
      const status = await session.call({ command: "status" }, 10_000);
      const semStatus = (status as any).semantic_index?.status;
      if (verbose) process.stdout.write(`    SEM-${backend} status: ${semStatus}\r`);
      if (semStatus === "ready" || semStatus === "partial") {
        if (verbose) process.stdout.write(`    SEM-${backend} status: ready     \n`);
        return session;
      }
      if (semStatus === "failed" || semStatus === "disabled") {
        const err = (status as any).semantic_index?.error || "unknown";
        if (verbose) process.stdout.write(`    SEM-${backend} status: ${semStatus} error=${err}     \n`);
        session.close();
        return null;
      }
      await new Promise((r) => setTimeout(r, 1000));
    }
    if (verbose) process.stdout.write(`    SEM-${backend} status: timeout   \n`);
    session.close();
    return null;
  } catch (e) {
    if (verbose) console.log(`    SEM-${backend} init ERROR: ${e}`);
    session.close();
    return null;
  }
}

async function semanticQuery(session: AftSession, query: string, k: number, run: SemanticRun, verbose: boolean): Promise<SearchResult[]> {
  try {
    const resp = await session.call({ command: "semantic_search", query, topK: k, ...run.request }, 30_000);
    const items = (resp as any).results;
    if (items && Array.isArray(items)) {
      const results = items.map((r: any) => ({
        file: r.file || r.file_path || r.path || "",
        line: r.start_line || r.line,
        score: r.score,
        snippet: r.snippet,
        start_line: r.start_line,
        end_line: r.end_line,
      }));
      // Log snippet sizes for token budget analysis
      const snippets = results.map((r) => r.snippet || "").filter((s) => s.length > 0);
      logChunkSizes(`sem-${run.backend}${run.modeSuffix}`, snippets, verbose);
      return results;
    }
  } catch (e) { if (verbose) console.log(`    SEM-${run.backend}${run.modeSuffix} ERROR: ${e}`); }
  return [];
}

// ---------------------------------------------------------------------------
// Hybrid search — FTS5 + semantic via RRF
// ---------------------------------------------------------------------------

function rrfFusion(fts5Results: SearchResult[], semResults: SearchResult[], k: number): SearchResult[] {
  const K = 60;
  const scoreMap = new Map<string, { result: SearchResult; score: number }>();

  fts5Results.forEach((r, i) => {
    const key = normalizePath(r.file);
    const existing = scoreMap.get(key);
    const s = 1 / (K + i + 1);
    if (existing) existing.score += s;
    else scoreMap.set(key, { result: r, score: s });
  });

  semResults.forEach((r, i) => {
    const key = normalizePath(r.file);
    const existing = scoreMap.get(key);
    const s = 1 / (K + i + 1);
    if (existing) existing.score += s;
    else scoreMap.set(key, { result: r, score: s });
  });

  return [...scoreMap.values()]
    .sort((a, b) => b.score - a.score)
    .slice(0, k)
    .map((v) => v.result);
}

// ---------------------------------------------------------------------------
// Ripgrep mode
// ---------------------------------------------------------------------------

function rgSearch(query: string, searchDir: string, benchmarkRoot: string | null, k: number): { results: SearchResult[]; latency_ms: number } {
  const targetDir = benchmarkRoot ? join(searchDir, benchmarkRoot) : searchDir;
  const start = performance.now();
  try {
    const output = execSync(`rg -n --no-heading --max-count ${k * 2} "${query.replace(/"/g, '\\"')}" .`, {
      cwd: targetDir, encoding: "utf-8", stdio: "pipe", timeout: 10000,
    });
    const results = output.trim().split("\n").filter(Boolean).slice(0, k).map((line) => {
      const ci = line.indexOf(":");
      const file = line.substring(0, ci);
      const rest = line.substring(ci + 1);
      const ci2 = rest.indexOf(":");
      return { file, line: parseInt(rest.substring(0, ci2), 10) };
    });
    return { results, latency_ms: performance.now() - start };
  } catch {
    return { results: [], latency_ms: performance.now() - start };
  }
}

// ---------------------------------------------------------------------------
// Auto-detect model from OpenAI-compatible endpoint
// ---------------------------------------------------------------------------

async function detectModel(url: string): Promise<string | null> {
  try {
    const resp = await fetch(`${url}/models`, { signal: AbortSignal.timeout(5000) });
    if (!resp.ok) return null;
    const json = await resp.json() as any;
    const models = json.data || json.models || [];
    if (models.length > 0) return models[0].id || models[0].name || null;
  } catch {}
  return null;
}

// ---------------------------------------------------------------------------
// Print terminal header
// ---------------------------------------------------------------------------

function printHeader(opts: {
  backends: string[]; k: number; rerank: boolean; rerankModel: string;
  rerankUrl: string; apiUrl?: string; apiModel?: string;
  semanticCount: number; lexicalCount: number; repos: string[];
  discovery?: ModelDiscoveryResult;
  m2vModel?: string; feModel?: string; oversample?: number;
  profileName: string; identifierSemantic: boolean;
  contextMode: ContextBenchmarkMode; contextBudget: ContextBudgetOptions;
}) {
  const W = "\x1b[1;37m", D = "\x1b[0;90m", N = "\x1b[0m";
  const bar = "═".repeat(65);
  console.log(`\n${W}${bar}${N}`);
  console.log(`${W}  AFT Search Benchmark${N}`);
  console.log(`${W}${bar}${N}`);
  console.log(`${D}  profile=${opts.profileName}  k=${opts.k}  repos=${opts.repos.join(", ")}  queries=${opts.semanticCount} NL + ${opts.lexicalCount} identifier${opts.rerank ? `  oversample=${opts.oversample}` : ""}${N}`);
  console.log(`${D}  identifier suites: lexical-symbol modes${opts.identifierSemantic ? " + semantic comparison" : " (semantic comparison off)"}${N}`);
  const budgetBits = [
    opts.contextBudget.totalTokens !== undefined ? `total=${opts.contextBudget.totalTokens}` : "",
    opts.contextBudget.perChunkTokens !== undefined ? `per_chunk=${opts.contextBudget.perChunkTokens}` : "",
    opts.contextBudget.softOverflowTokens !== undefined ? `soft_overflow=${opts.contextBudget.softOverflowTokens}` : "",
  ].filter(Boolean).join(" ");
  console.log(`${D}  semantic context: ${opts.contextMode}${budgetBits ? ` (${budgetBits})` : ""}${N}`);
  if (opts.contextMode === "legacy") {
    console.log(`${D}  note: legacy public semantic_search display snippets are rank-tiered: top hit fuller, ranks 2-3 short, rank 4+ path/header only${N}`);
  }

  // Use discovered models if available, otherwise fall back to hardcoded
  if (opts.discovery && opts.discovery.models.length > 0) {
    console.log(`\n${D}  Discovered Models (${opts.discovery.endpoint}):${N}`);
    for (const line of formatDiscoveredModels(opts.discovery)) {
      console.log(`${D}    ${line}${N}`);
    }
  } else {
    console.log(`\n${D}  Semantic Providers:${N}`);
    if (opts.backends.includes("model2vec")) console.log(`${D}    model2vec:   ${opts.m2vModel || "minishlab/potion-code-16M"} (512-dim static embeddings)${N}`);
    if (opts.backends.includes("fastembed")) console.log(`${D}    fastembed:   ${opts.feModel || "all-MiniLM-L6-v2"} (384-dim transformer, ONNX)${N}`);
    if (opts.backends.includes("semantic-api") && opts.apiUrl) console.log(`${D}    semantic-api: ${opts.apiModel || "?"} @ ${opts.apiUrl}${N}`);
    if (opts.rerank) console.log(`${D}  Reranker:      ${opts.rerankModel} @ ${opts.rerankUrl} (5x oversampling)${N}`);
  }

  // Tool-command mapping reference
  console.log(`\n${D}  Mode → AFT Tool → Rust Command:${N}`);
  console.log(`${D}    rg                    → bash (ripgrep)       → (external)${N}`);
  console.log(`${D}    aft-grep              → grep                 → grep${N}`);
  console.log(`${D}    fts5_search           → aft_fts5_search       → fts5_search${N}`);
  console.log(`${D}    fts5_find_symbol_*    → aft_find_symbol       → fts5_find_symbol${N}`);
  console.log(`${D}    glob                  → glob                  → glob${N}`);
  console.log(`${D}    ast_search            → ast_grep_search       → ast_search${N}`);
  console.log(`${D}    semantic_*            → aft_search            → semantic_search${N}`);
  console.log(`${D}    hybrid                → aft_search + fts5     → semantic_search + fts5_search (RRF)${N}`);
  console.log(`${D}    rerank                → aft_search + /v1/rerank → semantic_search + rerank endpoint${N}`);
  console.log("");
}

// ---------------------------------------------------------------------------
// Print results table
// ---------------------------------------------------------------------------

function printTable(title: string, metrics: AggregateMode[], rerankMetrics?: Record<string, RerankMetrics>) {
  const W = "\x1b[1;37m", G = "\x1b[0;32m", Y = "\x1b[0;33m", N = "\x1b[0m";
  const bar = "═".repeat(95);
  console.log(`\n${W}${bar}${N}`);
  console.log(`${W}  ${title}${N}`);
  console.log(`${W}${bar}${N}`);
  console.log(`  ${"Mode".padEnd(24)} ${"Recall".padStart(7)} ${"MRR".padStart(7)} ${"nDCG".padStart(7)} ${"Tok/q".padStart(7)} ${"Snip/q".padStart(7)} ${"p50(ms)".padStart(8)} ${"p95(ms)".padStart(8)} ${"Queries".padStart(9)}`);
  console.log(`  ${"─".repeat(24)} ${"─".repeat(7)} ${"─".repeat(7)} ${"─".repeat(7)} ${"─".repeat(7)} ${"─".repeat(7)} ${"─".repeat(8)} ${"─".repeat(8)} ${"─".repeat(9)}`);

  for (const m of metrics) {
    const recall = `${(m.recall * 100).toFixed(1)}%`;
    const mrr = m.mrr.toFixed(3);
    const ndcg = m.ndcg.toFixed(3);
    const tokens = m.tokens_per_query.toFixed(0);
    const snippets = m.snippets_per_query.toFixed(1);
    const p50 = m.p50_ms.toFixed(0);
    const p95 = m.p95_ms.toFixed(0);
    const queries = m.empty > 0 ? `${m.count}/${m.count + m.empty}` : `${m.count}/${m.count}`;
    const color = m.recall > 0.6 ? G : m.recall > 0.3 ? Y : "";
    console.log(`  ${color}${m.mode.padEnd(24)}${N} ${recall.padStart(7)} ${mrr.padStart(7)} ${ndcg.padStart(7)} ${tokens.padStart(7)} ${snippets.padStart(7)} ${p50.padStart(8)} ${p95.padStart(8)} ${queries.padStart(9)}`);
  }

  if (rerankMetrics) {
    console.log(`\n  ${"Rerank Delta".padEnd(24)} ${"Pre-Recall".padStart(10)} ${"Post-Recall".padStart(11)} ${"Post-MRR".padStart(9)} ${"Post-nDCG".padStart(10)} ${"ΔnDCG".padStart(7)} ${"p50(ms)".padStart(8)}`);
    console.log(`  ${"─".repeat(24)} ${"─".repeat(10)} ${"─".repeat(11)} ${"─".repeat(9)} ${"─".repeat(10)} ${"─".repeat(7)} ${"─".repeat(8)}`);
    for (const [mode, rm] of Object.entries(rerankMetrics)) {
      console.log(`  ${mode.padEnd(24)} ${(rm.pre_rerank_recall * 100).toFixed(1).padStart(9)}% ${(rm.post_rerank_recall * 100).toFixed(1).padStart(10)}% ${rm.post_rerank_mrr.toFixed(3).padStart(9)} ${rm.post_rerank_ndcg.toFixed(3).padStart(10)} ${rm.rerank_delta_ndcg >= 0 ? "+" : ""}${rm.rerank_delta_ndcg.toFixed(3).padStart(6)} ${rm.rerank_p50_ms.toFixed(0).padStart(8)}`);
    }
  }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

async function main() {
  const args = process.argv.slice(2);
  let cacheDir = ".bench-cache";
  let k = 10;
  let outputFile = "pilot-report.json";
  let binaryPath: string | null = null;
  let verbose = false;
  let semanticModel = "minishlab/potion-code-16M";
  let semanticBackend = "both";
  let apiUrl = "";
  let apiModel = "";
  let doRerank = false;
  let rerankModel = "GTE-Reranker-Modernbert";
  let queryPrompt: string | undefined;
  let oversample = 10;
  let rerankUrl = "http://127.0.0.1:8090/v1/rerank";
  let includeLexical = true;
  let interactive = false;
  let rerankContext = "aft_output"; // default: aft_output (WARNING 3)
  let profileName = "quick";
  let identifierSemanticExplicit: boolean | undefined;
  let contextMode: ContextBenchmarkMode = "legacy";
  const contextBudget: ContextBudgetOptions = {};
  const repoFilters = new Set<string>();

  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--cache-dir": cacheDir = args[++i]; break;
      case "--profile": profileName = args[++i]; break;
      case "--repo": repoFilters.add(normalizeRepoArg(args[++i])); break;
      case "--k": k = parseInt(args[++i], 10); break;
      case "--output": outputFile = args[++i]; break;
      case "--binary": binaryPath = args[++i]; break;
      case "--verbose": case "-v": verbose = true; break;
      case "--model": semanticModel = args[++i]; break;
      case "--backend": semanticBackend = args[++i]; break;
      case "--semantic-api-url": apiUrl = args[++i]; break;
      case "--semantic-api-model": apiModel = args[++i]; break;
      case "--rerank": doRerank = true; break;
      case "--rerank-model": rerankModel = args[++i]; break;
      case "--rerank-url": rerankUrl = args[++i]; break;
      case "--query-prompt": queryPrompt = args[++i]; break;
      case "--oversample": oversample = parseInt(args[++i], 10) || 10; break;
      case "--rerank-instruction": RERANK_INSTRUCTION = args[++i]; break;
      case "--context-mode": {
        const val = args[++i] as ContextBenchmarkMode;
        if (val !== "legacy" && val !== "budget" && val !== "compare") {
          console.error(`Error: unknown --context-mode: "${val}". Valid modes: legacy, budget, compare`);
          process.exit(1);
        }
        contextMode = val;
        break;
      }
      case "--context-total-tokens": contextBudget.totalTokens = parseInt(args[++i], 10); break;
      case "--context-per-chunk-tokens": contextBudget.perChunkTokens = parseInt(args[++i], 10); break;
      case "--context-soft-overflow-tokens": contextBudget.softOverflowTokens = parseInt(args[++i], 10); break;
      case "--rerank-context": {
        const val = args[++i];
        if (val !== "aft_output" && val !== "benchmark_enriched" && val !== "path_only") {
          console.error(`Error: unknown --rerank-context mode: "${val}". Valid modes: aft_output, benchmark_enriched, path_only`);
          process.exit(1);
        }
        rerankContext = val;
        break;
      }
      case "--include-lexical": includeLexical = args[++i] !== "false"; break;
      case "--identifier-semantic": identifierSemanticExplicit = args[++i] !== "false"; break;
      case "--interactive": interactive = true; break;
      case "--help": case "-h":
        console.log("Usage: bun run benchmarks/semble/pilot.ts --binary <path> [options]");
        console.log("  --profile <name>          smoke|quick|full");
        console.log("  --repo <name>             Limit to repo name or owner/name, e.g. aft or cortexkit/aft");
        console.log("  --k, --backend, --rerank, --semantic-api-url, --verbose");
        console.log("  --oversample <n>           Reranker oversampling multiplier (default: 10)");
        console.log("  --rerank-context <mode>    Reranker context mode: aft_output (default), benchmark_enriched, path_only");
        console.log("  --rerank-instruction <txt> Instruction prompt for reranker model");
        console.log("  --query-prompt <txt>       Query prompt template for embedding model");
        console.log("  --identifier-semantic <bool> Include semantic backends in identifier suites");
        console.log("  --context-mode <mode>      legacy|budget|compare semantic context behavior (default: legacy)");
        console.log("  --context-total-tokens <n> Total context token budget for budget/compare modes");
        console.log("  --context-per-chunk-tokens <n> Per chunk token budget for budget/compare modes");
        console.log("  --context-soft-overflow-tokens <n> Allow one chunk to overflow total budget by this much");
        process.exit(0);
    }
  }

  if (profileName === "smoke" && !args.includes("--k")) {
    k = 5;
  }
  const identifierSemantic = shouldRunIdentifierSemantic(profileName, identifierSemanticExplicit);

  // Normalize rerank URL: append /v1/rerank if not present
  if (rerankUrl && !rerankUrl.includes("/v1/rerank") && !rerankUrl.includes("/rerank")) {
    rerankUrl = rerankUrl.replace(/\/+$/, "") + "/v1/rerank";
  }

  RERANK_MODEL = rerankModel;
  RERANK_URL = rerankUrl;
  (globalThis as any).__SEMANTIC_API_URL = apiUrl;
  (globalThis as any).__SEMANTIC_API_MODEL = apiModel;

  const bin = binaryPath || "aft";
  if (binaryPath) {
    try { statSync(binaryPath); } catch {
      console.error(`ERROR: AFT binary not found at: ${binaryPath}`);
      process.exit(1);
    }
  }

  // Auto-detect semantic-api model if URL provided but model not specified
  // Skip if --interactive (handled later) or if model already specified
  if (apiUrl && !apiModel && !interactive) {
    console.log(`  Detecting model from ${apiUrl}...`);
    const detected = await detectModel(apiUrl);
    if (detected) { apiModel = detected; (globalThis as any).__SEMANTIC_API_MODEL = detected; }
    else console.warn(`  WARNING: Could not auto-detect model from ${apiUrl}. Pass --semantic-api-model or use --interactive.`);
  }

  // Load semantic NL fixtures
  const fixture = JSON.parse(readFileSync(resolve("benchmarks/semble/fixtures.json"), "utf-8"));
  const allAnnotations: Array<any> = [];
  const queryHoldOut = new Map<string, boolean>(); // query text → hold_out
  for (const repo of fixture.repos) {
    if (repoFilters.size > 0 && !repoFilters.has(repo.name)) continue;
    const annPath = resolve(`benchmarks/semble/annotations/${repo.name}.json`);
    if (!existsSync(annPath)) continue;
    const anns = JSON.parse(readFileSync(annPath, "utf-8"));
    for (const ann of anns) allAnnotations.push({ ...ann, repo_name: repo.name, _type: "semantic" });
  }

  // Build query → hold_out lookup from annotations
  for (const ann of allAnnotations) {
    queryHoldOut.set(ann.query, ann.hold_out === true);
  }

  const canonDir = resolve("benchmarks/semble/canon");
  const lexicalQueries = includeLexical
    ? buildLexicalQueriesFromCanon(canonDir, repoFilters)
    : [];

  // Build backends list (split comma-separated)
  const backends: string[] = [];
  for (const b of semanticBackend.split(',')) {
    const trimmed = b.trim();
    if (trimmed === "skip") continue;
    if (trimmed === "both") { backends.push("model2vec", "fastembed"); continue; }
    if (trimmed === "semantic-api") {
      if (apiUrl) backends.push("semantic-api");
      else console.warn("  WARNING: semantic-api requested but --semantic-api-url not set");
      continue;
    }
    backends.push(trimmed);
  }
  const semanticRuns = buildSemanticRuns(backends, contextMode, contextBudget);

  // Run preflight checks
  if (existsSync(canonDir)) {
    const preflightConfig = {
      profile: { name: profileName, allow_seed_canon: includeLexical } as any,
      profileName,
      suites: ["semantic_nl", "identifier_exact", "identifier_prefix", "path_lookup", "structural"] as any[],
      modes: [] as any[],
      binaryPath: bin,
      k,
      candidatePool: 50,
      rerankPool: 50,
      repetitions: 1,
      warmups: 1,
      backends,
      semanticModel,
      semanticApiUrl: apiUrl,
      semanticApiModel: apiModel,
      doRerank,
      rerankModel,
      rerankUrl,
      allowDegraded: true,
      allowSeedCanon: includeLexical,
      autoClone: false,
      verbose,
      reportJson: outputFile,
      reportJsonl: null,
      reportMd: null,
      cacheDir,
      includeLexical,
    };
    const preflightResults = runPreflight(preflightConfig, canonDir);
    if (preflightResults.length > 0) {
      printPreflight(preflightResults);
    }
  }

  // Collect all repos
  const allRepos = new Set<string>();
  for (const ann of allAnnotations) allRepos.add(ann.repo_name);
  for (const lq of lexicalQueries) allRepos.add(lq.repo_name);

  // Discover models from API endpoints if available
  let discovery: ModelDiscoveryResult | undefined;
  if (apiUrl) {
    // Interactive mode: discover and let user select models
    if (interactive && !apiModel) {
      const interactiveResult = await interactiveModelSelection(apiUrl, doRerank ? rerankUrl : undefined, verbose);
      if (!interactiveResult.proceed) {
        console.log("Benchmark cancelled by user.");
        process.exit(0);
      }
      if (interactiveResult.embeddingModel) {
        apiModel = interactiveResult.embeddingModel.id;
        (globalThis as any).__SEMANTIC_API_MODEL = apiModel;
      }
      if (interactiveResult.rerankerModel) {
        rerankModel = interactiveResult.rerankerModel.id;
        RERANK_MODEL = rerankModel;
      }
    }

    // If user specified both models, skip full discovery (avoids unloading from GPU)
    if (apiModel && rerankModel && doRerank) {
      console.log(`  Verifying specified models (skipping full discovery to preserve GPU memory)...`);
      discovery = await verifySpecificModels(apiUrl, apiModel, rerankModel, verbose);
    } else if (apiModel) {
      console.log(`  Verifying embedding model ${apiModel}...`);
      discovery = await verifySpecificModels(apiUrl, apiModel, undefined, verbose);
    } else {
      // Full discovery only when no models specified
      console.log("  Discovering models from semantic API (this probes all models)...");
      discovery = await discoverModels(apiUrl, verbose);
      if (discovery.embedding_models.length > 0) {
        apiModel = discovery.embedding_models[0].id;
        (globalThis as any).__SEMANTIC_API_MODEL = apiModel;
        console.log(`  Auto-detected embedding model: ${apiModel} (dim=${discovery.embedding_models[0].vector_dim})`);
      }
      // Re-probe desired models to reload them into GPU after full discovery
      if (apiModel) await ensureModelLoaded(apiUrl, apiModel, "embedding", verbose);
    }
  }
  if (doRerank && rerankUrl) {
    if (rerankModel) {
      // Already verified above, just ensure it's loaded
      await ensureModelLoaded(rerankUrl, rerankModel, "reranker", verbose);
    } else {
      console.log("  Discovering models from rerank endpoint...");
      const rerankDiscovery = await discoverModels(rerankUrl, verbose);
      if (rerankDiscovery.reranker_models.length > 0) {
        const best = rerankDiscovery.reranker_models[0];
        rerankModel = best.id;
        RERANK_MODEL = rerankModel;
        console.log(`  Auto-detected reranker: ${best.id}`);
      }
    }
  }

  // Print header
  printHeader({
    backends, k, rerank: doRerank, rerankModel, rerankUrl,
    apiUrl: apiUrl || undefined, apiModel: apiModel || undefined,
    semanticCount: allAnnotations.length, lexicalCount: lexicalQueries.length,
    repos: [...allRepos],
    discovery,
    m2vModel: semanticModel,
    feModel: "all-MiniLM-L6-v2",
    oversample,
    profileName,
    identifierSemantic,
    contextMode,
    contextBudget,
  });

  // Check which repos are available, auto-clone missing ones
  const availableRepos = new Set<string>();
  const allRepoDefs = [...fixture.repos];
  for (const repoName of allRepos) {
    const repoDir = join(resolve(cacheDir), repoName);
    if (existsSync(repoDir)) {
      availableRepos.add(repoName);
    } else {
      // Auto-clone
      const def = allRepoDefs.find((r) => r.name === repoName);
      if (def && "url" in def) {
        console.log(`  Cloning ${repoName}...`);
        try {
          execSync(`git clone --depth 1 ${def.url} ${repoDir}`, { stdio: "pipe", timeout: 120_000 });
          availableRepos.add(repoName);
          console.log(`  ✓ ${repoName} cloned`);
        } catch (e) {
          console.warn(`  ✗ Failed to clone ${repoName}: ${e}`);
        }
      }
    }
  }
  const skippedRepos = [...allRepos].filter((r) => !availableRepos.has(r));
  if (skippedRepos.length > 0) {
    console.log(`  Skipping (unavailable): ${skippedRepos.join(", ")}`);
  }
  const runnableLexicalQueries = lexicalQueries.filter((q) => q.repos.some((repo) => availableRepos.has(repo)));

  // Run semantic NL queries
  const allResults: ModeResult[] = [];
  const rerankResults: Record<string, RerankMetrics> = {};

  // Per-query context quality tracking (accumulated during rerank)
  const perQueryCQ: Array<{ mode: string; cq: Partial<ContextQuality> & { recall_at_k: number; hold_out: boolean; intent: string; harness_lat_ms: number; engine_latency_ms: number } }> = [];

  // Sessions per repo
  const semSessions: Record<string, AftSession | null> = {};
  let fts5Session: AftSession | null = null;
  let grepSession: AftSession | null = null;
  let currentRepo = "";

  for (const ann of allAnnotations) {
    if (!availableRepos.has(ann.repo_name)) continue;
    const repo = fixture.repos.find((r: any) => r.name === ann.repo_name);
    if (!repo) continue;
    const repoDir = join(resolve(cacheDir), repo.name);
    const targetDir = repo.benchmark_root ? join(repoDir, repo.benchmark_root) : repoDir;

    // Init sessions when repo changes
    if (ann.repo_name !== currentRepo) {
      for (const s of Object.values(semSessions)) s?.close();
      for (const k of Object.keys(semSessions)) delete semSessions[k];
      fts5Session?.close();
      grepSession?.close();

      currentRepo = ann.repo_name;
      console.log(`\n  Initializing sessions for ${ann.repo_name}...`);

      for (const run of semanticRuns) {
        const storageDir = join(targetDir, `.aft-bench-${run.backend}-${run.variant}`);
        const beModel = run.backend === "fastembed" ? "all-MiniLM-L6-v2" : run.backend === "semantic-api" ? apiModel : semanticModel;
        semSessions[run.key] = await initSemanticSession(bin, targetDir, beModel, run.backend, verbose, storageDir, queryPrompt, run.retrievalIntelligenceV2);
      }
      fts5Session = await initFts5Session(bin, targetDir, verbose);
      grepSession = await initGrepSession(bin, targetDir, verbose);
    }

    const allRelevant = [
      ...(ann.relevant || []).map((r: any) => typeof r === "string" ? r : r.path || ""),
      ...(ann.secondary || []).map((r: any) => typeof r === "string" ? r : r.path || ""),
    ].filter(Boolean);

    // Ripgrep
    const rg = rgSearch(ann.query, repoDir, repo.benchmark_root, k);
    allResults.push(scoredRow({ mode: "lexical (rg)", query: ann.query, repo_name: ann.repo_name, category: ann.category, suite: "semantic_nl", latency_ms: rg.latency_ms, results: rg.results, relevant: allRelevant, k }));

    // FTS5
    const fts5Start = performance.now();
    const fts5Results = fts5Session ? await fts5Query(fts5Session, ann.query, k, verbose) : [];
    const fts5Latency = performance.now() - fts5Start;
    allResults.push(scoredRow({ mode: "fts5", query: ann.query, repo_name: ann.repo_name, category: ann.category, suite: "semantic_nl", latency_ms: fts5Latency, results: fts5Results, relevant: allRelevant, k }));
    if (fts5Results.length > 0) {
      perQueryCQ.push({
        mode: "fts5",
        cq: { candidate_pool_size: fts5Results.length, rerank_pool_size: fts5Results.length, snippet_count: 0, enriched_candidate_count: 0, path_only_count: fts5Results.length, unenriched_candidate_count: fts5Results.length, avg_doc_tokens: 0, max_doc_tokens: 0, total_doc_tokens: 0, pre_rerank_recall_at_pool: recallAtK(fts5Results, allRelevant, k), post_rerank_recall_at_k: recallAtK(fts5Results, allRelevant, k), lost_relevant_after_rerank: [], context_exhausted: null, reranker_skipped_reason: null },
        recall_at_k: recallAtK(fts5Results, allRelevant, k), hold_out: ann.hold_out === true, intent: ann.category || "unknown", harness_lat_ms: fts5Latency, engine_latency_ms: fts5Latency,
      });
    }

    // AFT grep
    const grepStart = performance.now();
    const grepResults = grepSession ? await grepQuery(grepSession, ann.query, k, verbose) : [];
    const grepLatency = performance.now() - grepStart;
    allResults.push(scoredRow({ mode: "aft-grep", query: ann.query, repo_name: ann.repo_name, category: ann.category, suite: "semantic_nl", latency_ms: grepLatency, results: grepResults, relevant: allRelevant, k }));
    if (grepResults.length > 0) {
      perQueryCQ.push({
        mode: "aft-grep",
        cq: { candidate_pool_size: grepResults.length, rerank_pool_size: grepResults.length, snippet_count: 0, enriched_candidate_count: 0, path_only_count: grepResults.length, unenriched_candidate_count: grepResults.length, avg_doc_tokens: 0, max_doc_tokens: 0, total_doc_tokens: 0, pre_rerank_recall_at_pool: recallAtK(grepResults, allRelevant, k), post_rerank_recall_at_k: recallAtK(grepResults, allRelevant, k), lost_relevant_after_rerank: [], context_exhausted: null, reranker_skipped_reason: null },
        recall_at_k: recallAtK(grepResults, allRelevant, k), hold_out: ann.hold_out === true, intent: ann.category || "unknown", harness_lat_ms: grepLatency, engine_latency_ms: grepLatency,
      });
    }

    // Semantic backends
    for (const run of semanticRuns) {
      const session = semSessions[run.key];
      const modeName = semanticModeName(run);
      if (!session) {
        allResults.push(scoredRow({ mode: modeName, query: ann.query, repo_name: ann.repo_name, category: ann.category, suite: "semantic_nl", latency_ms: 0, results: [], relevant: allRelevant, k, status: "unavailable", reason: "semantic session unavailable" }));
        continue;
      }

      // Base pass
      const semStart = performance.now();
      const semResults = await semanticQuery(session, ann.query, k, run, verbose);
      const semLatency = performance.now() - semStart;
      allResults.push(scoredRow({ mode: modeName, query: ann.query, repo_name: ann.repo_name, category: ann.category, suite: "semantic_nl", latency_ms: semLatency, results: semResults, relevant: allRelevant, k }));
      if (semResults.length > 0) {
        // Track per-query context quality for base semantic mode
        const baseSnippets = semResults.filter((r) => (r.snippet || "").length > 10).length;
        const baseTokenCounts = semResults.map((r) => approxTokens(r.snippet || ""));
        perQueryCQ.push({
          mode: modeName,
          cq: {
            candidate_pool_size: semResults.length,
            rerank_pool_size: k,
            snippet_count: baseSnippets,
            enriched_candidate_count: baseSnippets,
            path_only_count: semResults.length - baseSnippets,
            unenriched_candidate_count: semResults.length - baseSnippets,
            avg_doc_tokens: baseTokenCounts.length > 0 ? Math.round(baseTokenCounts.reduce((a, b) => a + b, 0) / baseTokenCounts.length) : 0,
            max_doc_tokens: baseTokenCounts.length > 0 ? Math.max(...baseTokenCounts) : 0,
            total_doc_tokens: baseTokenCounts.reduce((a, b) => a + b, 0),
            pre_rerank_recall_at_pool: recallAtK(semResults, allRelevant, k),
            post_rerank_recall_at_k: recallAtK(semResults, allRelevant, k),
            lost_relevant_after_rerank: [],
            context_exhausted: null,
            reranker_skipped_reason: null,
          },
          recall_at_k: recallAtK(semResults, allRelevant, k),
          hold_out: ann.hold_out === true,
          intent: ann.category || "unknown",
          harness_lat_ms: semLatency,
          engine_latency_ms: semLatency,
        });
      }

      // Hybrid: FTS5 + semantic RRF (FTS5 is optional — hybrid works with semantic-only)
      if (semResults.length > 0) {
        const hybridResults = fts5Results.length > 0
          ? rrfFusion(fts5Results, semResults, k)
          : semResults; // No FTS5 results — use semantic-only as hybrid
        const hybridRecall = recallAtK(hybridResults, allRelevant, k);
        allResults.push(scoredRow({ mode: `hybrid-${modeName.replace("semantic-", "")}`, query: ann.query, repo_name: ann.repo_name, category: ann.category, suite: "semantic_nl", latency_ms: fts5Latency + semLatency, results: hybridResults, relevant: allRelevant, k }));
        const hybridSnippets = hybridResults.filter((r) => (r.snippet || "").length > 10).length;
        const hybridTokenCounts = hybridResults.map((r) => approxTokens(r.snippet || ""));
        perQueryCQ.push({
          mode: `hybrid-${modeName.replace("semantic-", "")}`,
          cq: { candidate_pool_size: hybridResults.length, rerank_pool_size: k, snippet_count: hybridSnippets, enriched_candidate_count: hybridSnippets, path_only_count: hybridResults.length - hybridSnippets, unenriched_candidate_count: hybridResults.length - hybridSnippets, avg_doc_tokens: hybridTokenCounts.length > 0 ? Math.round(hybridTokenCounts.reduce((a, b) => a + b, 0) / hybridTokenCounts.length) : 0, max_doc_tokens: hybridTokenCounts.length > 0 ? Math.max(...hybridTokenCounts) : 0, total_doc_tokens: hybridTokenCounts.reduce((a, b) => a + b, 0), pre_rerank_recall_at_pool: hybridRecall, post_rerank_recall_at_k: hybridRecall, lost_relevant_after_rerank: [], context_exhausted: null, reranker_skipped_reason: null },
          recall_at_k: hybridRecall, hold_out: ann.hold_out === true, intent: ann.category || "unknown", harness_lat_ms: fts5Latency + semLatency, engine_latency_ms: semLatency,
        });
      } else {
        allResults.push(scoredRow({ mode: `hybrid-${modeName.replace("semantic-", "")}`, query: ann.query, repo_name: ann.repo_name, category: ann.category, suite: "semantic_nl", latency_ms: fts5Latency + semLatency, results: [], relevant: allRelevant, k }));
      }
    }

    // Rerank pass (for each semantic backend)
    if (doRerank) {
      for (const run of semanticRuns) {
        const session = semSessions[run.key];
        const modeName = semanticModeName(run);
        if (!session) {
          allResults.push(scoredRow({ mode: `${modeName}+rerank`, query: ann.query, repo_name: ann.repo_name, category: ann.category, suite: "semantic_nl", latency_ms: 0, results: [], relevant: allRelevant, k, status: "unavailable", reason: "semantic session unavailable" }));
          continue;
        }
        const rerankSemStart = performance.now();
        const semResults = await semanticQuery(session, ann.query, k * oversample, run, verbose);
        const rerankSemLatency = performance.now() - rerankSemStart;
        if (semResults.length === 0) {
          allResults.push(scoredRow({ mode: `${modeName}+rerank`, query: ann.query, repo_name: ann.repo_name, category: ann.category, suite: "semantic_nl", latency_ms: rerankSemLatency, results: [], relevant: allRelevant, k }));
          continue;
        }

        const preRecall = recallAtK(semResults, allRelevant, k * oversample);
        const { results: reranked, latency_ms: rerankLat, reranker_skipped_reason, enriched_candidate_count, path_only_count, snippet_count, avg_doc_tokens, max_doc_tokens, total_doc_tokens } = await applyRerank(ann.query, semResults, k, repoDir, verbose, oversample, rerankContext);
        const postRecall = recallAtK(reranked, allRelevant, k);
        const postMrr = mrr(reranked, allRelevant);
        const postNdcg = ndcgAtK(reranked, allRelevant, k);

        // Compute lost_relevant_after_rerank: relevant files in pre-rerank top-k but not in post-rerank top-k
        const preRerankTopK = semResults.slice(0, k);
        const postRerankTopK = reranked.slice(0, k);
        const preRerankRelevant = new Set(preRerankTopK.filter((r) => allRelevant.some((rel) => pathMatches(r.file, rel))).map((r) => normalizePath(r.file)));
        const postRerankRelevant = new Set(postRerankTopK.filter((r) => allRelevant.some((rel) => pathMatches(r.file, rel))).map((r) => normalizePath(r.file)));
        const lostRelevant = [...preRerankRelevant].filter((f) => !postRerankRelevant.has(f));

        allResults.push(scoredRow({ mode: `${modeName}+rerank`, query: ann.query, repo_name: ann.repo_name, category: ann.category, suite: "semantic_nl", latency_ms: rerankLat, results: reranked, relevant: allRelevant, k }));

        // Track per-query context quality for rerank modes
        perQueryCQ.push({
          mode: `${modeName}+rerank`,
          cq: {
            candidate_pool_size: semResults.length,
            rerank_pool_size: Math.min(k * oversample, semResults.length),
            snippet_count,
            enriched_candidate_count,
            path_only_count,
            unenriched_candidate_count: path_only_count,
            avg_doc_tokens,
            max_doc_tokens,
            total_doc_tokens,
            pre_rerank_recall_at_pool: preRecall,
            post_rerank_recall_at_k: postRecall,
            lost_relevant_after_rerank: lostRelevant,
            context_exhausted: null,
            reranker_skipped_reason: reranker_skipped_reason || null,
          },
          recall_at_k: postRecall,
          hold_out: ann.hold_out === true,
          intent: ann.category || "unknown",
          harness_lat_ms: rerankSemLatency + rerankLat,
          engine_latency_ms: rerankSemLatency,
        });

        // Track rerank metrics
        const key = `${modeName}+rerank`;
        if (!rerankResults[key]) rerankResults[key] = { pre_rerank_recall: 0, post_rerank_recall: 0, post_rerank_mrr: 0, post_rerank_ndcg: 0, rerank_delta_ndcg: 0, rerank_p50_ms: 0, rerank_p95_ms: 0 };
        // We'll aggregate below
      }
    }
  }

  // Run lexical identifier queries
  if (includeLexical) {
    for (const [repoName, repoQueries] of groupLexicalQueriesByRepo(runnableLexicalQueries)) {
      if (!availableRepos.has(repoName)) continue;
      const repoDir = join(resolve(cacheDir), repoName);
      const lexicalSessions: Record<string, AftSession | null> = {};

      console.log(`\n  Initializing lexical sessions for ${repoName}...`);
      lexicalSessions["aft-grep"] = await initGrepSession(bin, repoDir, verbose);
      lexicalSessions["fts5"] = await initFts5Session(bin, repoDir, verbose);

      if (identifierSemantic) {
        for (const run of semanticRuns) {
          const storageDir = join(repoDir, `.aft-bench-${run.backend}-${run.variant}-lex`);
          const beModel = run.backend === "fastembed" ? "all-MiniLM-L6-v2" : run.backend === "semantic-api" ? apiModel : semanticModel;
          lexicalSessions[run.key] = await initSemanticSession(bin, repoDir, beModel, run.backend, verbose, storageDir, queryPrompt, run.retrievalIntelligenceV2);
        }
      }

      for (const lq of repoQueries) {
        const allRelevant = [...lq.relevant, ...lq.secondary];

        // Ripgrep (baseline contestant, NOT oracle)
        const rg = rgSearch(lq.query, repoDir, null, k);
        allResults.push(scoredRow({ mode: "lexical (rg)", query: lq.query, repo_name: repoName, category: lq.category, suite: lq.suite, latency_ms: rg.latency_ms, results: rg.results, relevant: allRelevant, k }));

        // AFT grep
        const grepS = lexicalSessions["aft-grep"];
        if (grepS) {
          const gs = performance.now();
          const gr = await grepQuery(grepS, lq.query, k, verbose);
          allResults.push(scoredRow({ mode: "aft-grep", query: lq.query, repo_name: repoName, category: lq.category, suite: lq.suite, latency_ms: performance.now() - gs, results: gr, relevant: allRelevant, k }));
        } else {
          allResults.push(scoredRow({ mode: "aft-grep", query: lq.query, repo_name: repoName, category: lq.category, suite: lq.suite, latency_ms: 0, results: [], relevant: allRelevant, k, status: "unavailable", reason: "grep session unavailable" }));
        }

        // FTS5
        const f5S = lexicalSessions["fts5"];
        if (f5S) {
          const fs = performance.now();
          const f5r = await fts5Query(f5S, lq.query, k, verbose);
          allResults.push(scoredRow({ mode: "fts5", query: lq.query, repo_name: repoName, category: lq.category, suite: lq.suite, latency_ms: performance.now() - fs, results: f5r, relevant: allRelevant, k }));
        } else {
          allResults.push(scoredRow({ mode: "fts5", query: lq.query, repo_name: repoName, category: lq.category, suite: lq.suite, latency_ms: 0, results: [], relevant: allRelevant, k, status: "unavailable", reason: "FTS5 session unavailable" }));
        }

        // Dedicated FTS5 symbol lookup
        if (f5S) {
          const symbolMode = lq.suite === "identifier_exact" ? "exact" : "prefix";
          const symbolModeName = symbolMode === "exact" ? "fts5_find_symbol_exact" : "fts5_find_symbol_prefix";
          const ss = performance.now();
          const symbols = await fts5FindSymbolQuery(f5S, lq.query, symbolMode, k, verbose);
          allResults.push(scoredRow({ mode: symbolModeName, query: lq.query, repo_name: repoName, category: lq.category, suite: lq.suite, latency_ms: performance.now() - ss, results: symbols, relevant: allRelevant, k }));
        } else {
          const symbolModeName = lq.suite === "identifier_exact" ? "fts5_find_symbol_exact" : "fts5_find_symbol_prefix";
          allResults.push(scoredRow({ mode: symbolModeName, query: lq.query, repo_name: repoName, category: lq.category, suite: lq.suite, latency_ms: 0, results: [], relevant: allRelevant, k, status: "unavailable", reason: "FTS5 session unavailable" }));
        }

        // Semantic backends
        if (identifierSemantic) {
          for (const run of semanticRuns) {
            const session = lexicalSessions[run.key];
            const modeName = semanticModeName(run);
            if (!session) {
              allResults.push(scoredRow({ mode: modeName, query: lq.query, repo_name: repoName, category: lq.category, suite: lq.suite, latency_ms: 0, results: [], relevant: allRelevant, k, status: "unavailable", reason: "semantic session unavailable" }));
              continue;
            }
            const ss = performance.now();
            const sr = await semanticQuery(session, lq.query, k, run, verbose);
            allResults.push(scoredRow({ mode: modeName, query: lq.query, repo_name: repoName, category: lq.category, suite: lq.suite, latency_ms: performance.now() - ss, results: sr, relevant: allRelevant, k }));
          }
        }
      }
      for (const s of Object.values(lexicalSessions)) s?.close();
    }
  }

  // Aggregate results
  const byMode = new Map<string, ModeResult[]>();
  for (const r of allResults) {
    if (!byMode.has(r.mode)) byMode.set(r.mode, []);
    byMode.get(r.mode)!.push(r);
  }

  const suiteTotals: Record<string, number> = {
    semantic_nl: allAnnotations.filter((ann) => availableRepos.has(ann.repo_name)).length,
  };
  for (const lq of runnableLexicalQueries) {
    suiteTotals[lq.suite] = (suiteTotals[lq.suite] || 0) + lq.repos.filter((repo) => availableRepos.has(repo)).length;
  }
  const { semantic: semanticAgg, bySuite: suiteAggregates } = splitAggregatesBySuite(allResults, suiteTotals);

  // Compute rerank metrics
  const rerankAgg: Record<string, RerankMetrics> = {};
  for (const mode of Object.keys(rerankResults)) {
    const baseMode = mode.replace("+rerank", "");
    const baseRows = (byMode.get(baseMode) || []).filter((r) => r.suite === "semantic_nl");
    const rerankRows = (byMode.get(mode) || []).filter((r) => r.suite === "semantic_nl");
    if (baseRows.length === 0 || rerankRows.length === 0) continue;

    const preRecalls = baseRows.map((r) => r.recall_at_k);
    const postRecalls = rerankRows.map((r) => r.recall_at_k);
    const postMrrs = rerankRows.map((r) => r.mrr);
    const postNdcgs = rerankRows.map((r) => r.ndcg_at_k);
    const preNdcgs = baseRows.map((r) => r.ndcg_at_k);
    const rerankLats = rerankRows.map((r) => r.latency_ms).sort((a, b) => a - b);

    const n = rerankRows.length;
    rerankAgg[mode] = {
      pre_rerank_recall: preRecalls.reduce((s, v) => s + v, 0) / n,
      post_rerank_recall: postRecalls.reduce((s, v) => s + v, 0) / n,
      post_rerank_mrr: postMrrs.reduce((s, v) => s + v, 0) / n,
      post_rerank_ndcg: postNdcgs.reduce((s, v) => s + v, 0) / n,
      rerank_delta_ndcg: (postNdcgs.reduce((s, v) => s + v, 0) - preNdcgs.reduce((s, v) => s + v, 0)) / n,
      rerank_p50_ms: percentile(rerankLats, 50),
      rerank_p95_ms: percentile(rerankLats, 95),
    };
  }

  // Print tables
  if (semanticAgg.length > 0) printTable(`SEMANTIC_NL (natural-language queries, k=${k})`, semanticAgg, Object.keys(rerankAgg).length > 0 ? rerankAgg : undefined);
  for (const [suite, metrics] of Object.entries(suiteAggregates)) {
    if (suite === "semantic_nl" || metrics.length === 0) continue;
    printTable(`${suite.toUpperCase()} (identifier queries, k=${k})`, metrics);
  }

  // Compute context quality per mode
  const contextQualityByMode: Record<string, ContextQuality> = {};

  for (const [mode, rows] of byMode) {
    if (rows.length === 0) continue;

    // Check if we have per-query tracking data for this mode (rerank modes)
    const modeCQEntries = perQueryCQ.filter((e) => e.mode === mode);

    if (modeCQEntries.length > 0) {
      // Rerank mode: aggregate from per-query tracking
      const tuningRecalls: number[] = [];
      const holdoutRecalls: number[] = [];
      const intentDist: Record<string, number> = {};
      const allLostRelevant: string[] = [];
      let totalCandidatePool = 0;
      let totalRerankPool = 0;
      let totalSnippetCount = 0;
      let totalEnriched = 0;
      let totalPathOnly = 0;
      let totalAvgDocTokens = 0;
      let maxDocTokens = 0;
      let totalDocTokens = 0;
      const engineLats: number[] = [];
      const harnessLats: number[] = [];
      let skippedCount = 0;

      for (const entry of modeCQEntries) {
        const c = entry.cq;
        if (entry.hold_out) holdoutRecalls.push(entry.recall_at_k);
        else tuningRecalls.push(entry.recall_at_k);

        intentDist[entry.intent] = (intentDist[entry.intent] || 0) + 1;
        allLostRelevant.push(...(c.lost_relevant_after_rerank || []));
        totalCandidatePool += c.candidate_pool_size || 0;
        totalRerankPool += c.rerank_pool_size || 0;
        totalSnippetCount += c.snippet_count || 0;
        totalEnriched += c.enriched_candidate_count || 0;
        totalPathOnly += c.path_only_count || 0;
        totalAvgDocTokens += c.avg_doc_tokens || 0;
        maxDocTokens = Math.max(maxDocTokens, c.max_doc_tokens || 0);
        totalDocTokens += c.total_doc_tokens || 0;
        engineLats.push(entry.engine_latency_ms || 0);
        harnessLats.push(entry.harness_lat_ms || 0);
        if (c.reranker_skipped_reason) skippedCount++;
      }

      const n = modeCQEntries.length;
      const tuningRecall10 = tuningRecalls.length > 0
        ? tuningRecalls.reduce((s, v) => s + v, 0) / tuningRecalls.length : 0;
      const holdoutRecall10 = holdoutRecalls.length > 0
        ? holdoutRecalls.reduce((s, v) => s + v, 0) / holdoutRecalls.length : 0;

      contextQualityByMode[mode] = {
        candidate_pool_size: Math.round(totalCandidatePool / n),
        rerank_pool_size: Math.round(totalRerankPool / n),
        snippet_count: totalSnippetCount,
        enriched_candidate_count: Math.round(totalEnriched / n),
        path_only_count: Math.round(totalPathOnly / n),
        unenriched_candidate_count: Math.round(totalPathOnly / n),
        avg_doc_tokens: n > 0 ? Math.round(totalAvgDocTokens / n) : 0,
        max_doc_tokens: maxDocTokens,
        total_doc_tokens: totalDocTokens,
        pre_rerank_recall_at_pool: modeCQEntries.reduce((s, e) => s + (e.cq.pre_rerank_recall_at_pool || 0), 0) / n,
        post_rerank_recall_at_k: modeCQEntries.reduce((s, e) => s + (e.cq.post_rerank_recall_at_k || 0), 0) / n,
        lost_relevant_after_rerank: [...new Set(allLostRelevant)],
        context_exhausted: modeCQEntries.some((e) => e.cq.context_exhausted === true),
        reranker_skipped_reason: skippedCount > 0 ? `${skippedCount}/${n} queries skipped` : null,
        intent_distribution: intentDist,
        tuning_recall_at_10: tuningRecall10,
        holdout_recall_at_10: holdoutRecall10,
        engine_latency_p95_ms: percentile([...engineLats].sort((a, b) => a - b), 95),
        benchmark_harness_latency_p95_ms: percentile([...harnessLats].sort((a, b) => a - b), 95),
      };
    } else {
      // Non-rerank mode: aggregate from results directly
      let totalSnippets = 0;
      let totalPathOnly = 0;
      let maxTokens = 0;
      let totalDocTokens = 0;
      const tuningRecalls: number[] = [];
      const holdoutRecalls: number[] = [];
      const allLatencies: number[] = [];
      const intentDist: Record<string, number> = {};

      for (const row of rows) {
        intentDist[row.category] = (intentDist[row.category] || 0) + 1;
        const results = row.results || [];
        let snippets = 0;
        let pathOnly = 0;
        for (const r of results) {
          const snippet = r.snippet || "";
          if (snippet.length > 10 && !snippet.includes("[budget_exhausted]")) {
            snippets++;
            const tokens = Math.ceil(snippet.length / 4);
            totalDocTokens += tokens;
            if (tokens > maxTokens) maxTokens = tokens;
          } else {
            pathOnly++;
          }
        }
        totalSnippets += snippets;
        totalPathOnly += pathOnly;
        allLatencies.push(row.latency_ms);

        // Use annotation hold_out field if available
        const isHoldout = queryHoldOut.get(row.query) === true;
        if (isHoldout) holdoutRecalls.push(row.recall_at_k);
        else tuningRecalls.push(row.recall_at_k);
      }

      const tuningRecall10 = tuningRecalls.length > 0
        ? tuningRecalls.reduce((s, v) => s + v, 0) / tuningRecalls.length : 0;
      const holdoutRecall10 = holdoutRecalls.length > 0
        ? holdoutRecalls.reduce((s, v) => s + v, 0) / holdoutRecalls.length : 0;
      const sortedLats = [...allLatencies].sort((a, b) => a - b);

      const n = rows.length;
      const avgResultsPerQuery = n > 0 ? Math.round(rows.reduce((s, r) => s + (r.results?.length || 0), 0) / n) : k;
      contextQualityByMode[mode] = {
        candidate_pool_size: avgResultsPerQuery,
        rerank_pool_size: avgResultsPerQuery,
        snippet_count: totalSnippets,
        enriched_candidate_count: n > 0 ? Math.round(totalSnippets / n) : 0,
        path_only_count: n > 0 ? Math.round(totalPathOnly / n) : 0,
        unenriched_candidate_count: n > 0 ? Math.round(totalPathOnly / n) : 0,
        avg_doc_tokens: totalSnippets > 0 ? Math.round(totalDocTokens / totalSnippets) : 0,
        max_doc_tokens: maxTokens,
        total_doc_tokens: totalDocTokens,
        pre_rerank_recall_at_pool: 0,
        post_rerank_recall_at_k: rows.reduce((s, r) => s + r.recall_at_k, 0) / rows.length,
        lost_relevant_after_rerank: [],
        context_exhausted: null,
        reranker_skipped_reason: null,
        intent_distribution: intentDist,
        tuning_recall_at_10: tuningRecall10,
        holdout_recall_at_10: holdoutRecall10,
        engine_latency_p95_ms: percentile(sortedLats, 95),
        benchmark_harness_latency_p95_ms: percentile(sortedLats, 95),
      };
    }
  }

  // Write JSON report
  const incompleteReasons: string[] = [];
  if (skippedRepos.length > 0) {
    incompleteReasons.push(`unavailable repos: ${skippedRepos.join(", ")}`);
  }
  const reportEmptyCounts = buildEmptyCounts(allResults);
  const emptyParts = Object.entries(reportEmptyCounts)
    .filter(([, v]) => v > 0)
    .map(([k, v]) => `${k}=${v}`);
  if (emptyParts.length > 0) {
    incompleteReasons.push(`empty result phases: ${emptyParts.join(" ")}`);
  }
  if (allResults.length === 0) {
    incompleteReasons.push("no benchmark results produced");
  }
  const intentMetrics = aggregateIntentMetrics(allResults, queryHoldOut);
  if (Object.keys(intentMetrics).length === 0) {
    incompleteReasons.push("intent_metrics is empty");
  }

  const report = {
    timestamp: new Date().toISOString(),
    status: incompleteReasons.length > 0 ? "incomplete" : "complete",
    incomplete_reasons: incompleteReasons,
    profile: profileName,
    repo_filter: [...repoFilters],
    k,
    binary: binaryPath,
    backends,
    identifier_semantic: identifierSemantic,
    context_mode: contextMode,
    context_budget: contextBudget,
    semantic_runs: semanticRuns.map((run) => ({
      key: run.key,
      backend: run.backend,
      variant: run.variant,
      retrieval_intelligence_v2: run.retrievalIntelligenceV2,
      request: run.request,
    })),
    semantic_snippet_display_policy: {
      mode: "rank_tiered_public_semantic_search",
      note: "AFT currently enriches public semantic_search snippets for the first three ranked semantic results only; rank 4+ results are path/header oriented even when k is larger.",
    },
    rerank: doRerank ? { model: rerankModel, url: rerankUrl } : null,
    rerank_context: rerankContext,
    suite_totals: suiteTotals,
    intent_metrics: intentMetrics,
    results: allResults,
    aggregate: Object.fromEntries(semanticAgg.map((a) => [a.mode, a])),
    lexical_aggregate: Object.fromEntries(
      Object.entries(suiteAggregates)
        .filter(([suite]) => suite !== "semantic_nl")
        .map(([suite, metrics]) => [suite, Object.fromEntries(metrics.map((a) => [a.mode, a]))]),
    ),
    suite_aggregates: Object.fromEntries(
      Object.entries(suiteAggregates).map(([suite, metrics]) => [suite, Object.fromEntries(metrics.map((a) => [a.mode, a]))]),
    ),
    rerank_metrics: rerankAgg,
    context_quality: contextQualityByMode,
    empty_counts: reportEmptyCounts,
  };
  const resolvedOutput = resolve(outputFile);
  mkdirSync(resolve(resolvedOutput, ".."), { recursive: true });
  writeFileSync(resolvedOutput, JSON.stringify(report, null, 2) + "\n");

  // Close sessions
  for (const s of Object.values(semSessions)) s?.close();
  fts5Session?.close();
  grepSession?.close();

  // Empty summary
  if (emptyParts.length > 0) console.log(`\n  ⚠ Empty results: ${emptyParts.join(" ")}`);
  if (incompleteReasons.length > 0) console.log(`\n  Incomplete benchmark: ${incompleteReasons.join("; ")}`);
  console.log(`\n  Report saved to ${resolvedOutput}`);
}

if (import.meta.main) {
  main();
}
