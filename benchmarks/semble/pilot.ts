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
 *   --output <file>          JSON report output path
 *   --verbose, -v            Per-query debug output
 */

import { readFileSync, writeFileSync, existsSync, statSync } from "fs";
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

interface ModeResult {
  mode: string;
  query: string;
  repo_name: string;
  category: string;
  latency_ms: number;
  results: SearchResult[];
  recall_at_k: number;
  mrr: number;
  ndcg_at_k: number;
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

function logChunkSizes(label: string, chunks: string[], verbose: boolean): void {
  if (!verbose || chunks.length === 0) return;
  const sizes = chunks.map(approxTokens);
  const over = sizes.filter((s) => s > 2048).length;
  const max = Math.max(...sizes);
  const avg = Math.round(sizes.reduce((a, b) => a + b, 0) / sizes.length);
  console.log(`    CHUNK-SIZE ${label}: ${chunks.length} chunks, avg=${avg} max=${max} over2048=${over}`);
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

function ndcgAtK(retrieved: SearchResult[], relevant: string[], k: number): number {
  if (!retrieved) return 0;
  const relSet = new Set(relevant.map(normalizePath));
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

function percentile(sorted: number[], p: number): number {
  if (sorted.length === 0) return 0;
  const idx = Math.ceil(sorted.length * p / 100) - 1;
  return sorted[Math.max(0, idx)];
}

function aggregateMetrics(rows: ModeResult[], totalQueries: number): AggregateMode {
  const mode = rows.length > 0 ? rows[0].mode : "unknown";
  const n = rows.length;
  const latencies = rows.map((r) => r.latency_ms).sort((a, b) => a - b);
  return {
    mode,
    recall: n > 0 ? rows.reduce((s, r) => s + r.recall_at_k, 0) / n : 0,
    mrr: n > 0 ? rows.reduce((s, r) => s + r.mrr, 0) / n : 0,
    ndcg: n > 0 ? rows.reduce((s, r) => s + r.ndcg_at_k, 0) / n : 0,
    p50_ms: percentile(latencies, 50),
    p95_ms: percentile(latencies, 95),
    count: n,
    empty: totalQueries - n,
  };
}

// ---------------------------------------------------------------------------
// Lexical queries (ported from search-bench-v2.py)
// ---------------------------------------------------------------------------

const LEXICAL_QUERIES = [
  { query: "validate_path", repos: ["aft"], category: "identifier" },
  { query: "BinaryBridge", repos: ["aft"], category: "identifier" },
  { query: "fn handle_grep", repos: ["aft"], category: "identifier" },
  { query: "experimental_search_index", repos: ["aft"], category: "identifier" },
  { query: "BlockNumber", repos: ["reth"], category: "identifier" },
  { query: "fn execute", repos: ["reth"], category: "identifier" },
  { query: "EthApiError", repos: ["reth"], category: "identifier" },
  { query: "impl Display for", repos: ["reth"], category: "identifier" },
];

const LEXICAL_REPOS = [
  { name: "aft", language: "rust", url: "https://github.com/cortexkit/aft.git", benchmark_root: null },
  { name: "reth", language: "rust", url: "https://github.com/paradigmxyz/reth.git", benchmark_root: null },
];

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

// ---------------------------------------------------------------------------
// Semantic mode — persistent session per repo
// ---------------------------------------------------------------------------

async function initSemanticSession(
  bin: string, targetDir: string, model: string, backend: string,
  verbose: boolean, storageDir?: string, queryPromptOverride?: string,
): Promise<AftSession | null> {
  const session = new AftSession(bin);
  try {
    const config: Record<string, unknown> = {
      command: "configure", harness: "opencode",
      project_root: targetDir,
      storage_dir: storageDir || join(targetDir, ".aft-bench"),
      semantic_search: true,
    };
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

async function semanticQuery(session: AftSession, query: string, k: number, backend: string, verbose: boolean): Promise<SearchResult[]> {
  try {
    const resp = await session.call({ command: "semantic_search", query, topK: k }, 30_000);
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
      logChunkSizes(`sem-${backend}`, snippets, verbose);
      return results;
    }
  } catch (e) { if (verbose) console.log(`    SEM-${backend} ERROR: ${e}`); }
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
}) {
  const W = "\x1b[1;37m", D = "\x1b[0;90m", N = "\x1b[0m";
  const bar = "═".repeat(65);
  console.log(`\n${W}${bar}${N}`);
  console.log(`${W}  AFT Search Benchmark${N}`);
  console.log(`${W}${bar}${N}`);
  console.log(`${D}  k=${opts.k}  repos=${opts.repos.join(", ")}  queries=${opts.semanticCount} NL + ${opts.lexicalCount} identifier${opts.rerank ? `  oversample=${opts.oversample}` : ""}${N}`);

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
  console.log(`  ${"Mode".padEnd(24)} ${"Recall".padStart(7)} ${"MRR".padStart(7)} ${"nDCG".padStart(7)} ${"p50(ms)".padStart(8)} ${"p95(ms)".padStart(8)} ${"Queries".padStart(9)}`);
  console.log(`  ${"─".repeat(24)} ${"─".repeat(7)} ${"─".repeat(7)} ${"─".repeat(7)} ${"─".repeat(8)} ${"─".repeat(8)} ${"─".repeat(9)}`);

  for (const m of metrics) {
    const recall = `${(m.recall * 100).toFixed(1)}%`;
    const mrr = m.mrr.toFixed(3);
    const ndcg = m.ndcg.toFixed(3);
    const p50 = m.p50_ms.toFixed(0);
    const p95 = m.p95_ms.toFixed(0);
    const queries = m.empty > 0 ? `${m.count}/${m.count + m.empty}` : `${m.count}/${m.count}`;
    const color = m.recall > 0.6 ? G : m.recall > 0.3 ? Y : "";
    console.log(`  ${color}${m.mode.padEnd(24)}${N} ${recall.padStart(7)} ${mrr.padStart(7)} ${ndcg.padStart(7)} ${p50.padStart(8)} ${p95.padStart(8)} ${queries.padStart(9)}`);
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

  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--cache-dir": cacheDir = args[++i]; break;
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
      case "--interactive": interactive = true; break;
      case "--help": case "-h":
        console.log("Usage: bun run benchmarks/semble/pilot.ts --binary <path> [options]");
        console.log("  --k, --backend, --rerank, --semantic-api-url, --verbose");
        console.log("  --oversample <n>           Reranker oversampling multiplier (default: 10)");
        console.log("  --rerank-context <mode>    Reranker context mode: aft_output (default), benchmark_enriched, path_only");
        console.log("  --rerank-instruction <txt> Instruction prompt for reranker model");
        console.log("  --query-prompt <txt>       Query prompt template for embedding model");
        process.exit(0);
    }
  }

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
  for (const repo of fixture.repos) {
    const annPath = resolve(`benchmarks/semble/annotations/${repo.name}.json`);
    if (!existsSync(annPath)) continue;
    const anns = JSON.parse(readFileSync(annPath, "utf-8"));
    for (const ann of anns) allAnnotations.push({ ...ann, repo_name: repo.name, _type: "semantic" });
  }

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

  // Run preflight checks
  const canonDir = resolve("benchmarks/semble/canon");
  if (existsSync(canonDir)) {
    const preflightConfig = {
      profile: { name: "quick", allow_seed_canon: includeLexical } as any,
      profileName: "quick",
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
  for (const lr of LEXICAL_REPOS) allRepos.add(lr.name);

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
    semanticCount: allAnnotations.length, lexicalCount: includeLexical ? LEXICAL_QUERIES.length : 0,
    repos: [...allRepos],
    discovery,
    m2vModel: semanticModel,
    feModel: "all-MiniLM-L6-v2",
    oversample,
  });

  // Check which repos are available, auto-clone missing ones
  const availableRepos = new Set<string>();
  const allRepoDefs = [...fixture.repos, ...LEXICAL_REPOS];
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

  // Run semantic NL queries
  const allResults: ModeResult[] = [];
  const rerankResults: Record<string, RerankMetrics> = {};
  const emptyCounts: Record<string, number> = {};

  // Per-query context quality tracking (accumulated during rerank)
  const perQueryCQ: Array<{ mode: string; cq: Partial<ContextQuality> & { recall_at_k: number; hold_out: boolean; intent: string; harness_lat_ms: number } }> = [];

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

      for (const be of backends) {
        const storageDir = join(targetDir, `.aft-bench-${be}`);
        const beModel = be === "fastembed" ? "all-MiniLM-L6-v2" : be === "semantic-api" ? apiModel : semanticModel;
        semSessions[be] = await initSemanticSession(bin, targetDir, beModel, be, verbose, storageDir, queryPrompt);
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
    allResults.push({ mode: "lexical (rg)", query: ann.query, repo_name: ann.repo_name, category: ann.category, latency_ms: rg.latency_ms, results: rg.results, recall_at_k: recallAtK(rg.results, allRelevant, k), mrr: mrr(rg.results, allRelevant), ndcg_at_k: ndcgAtK(rg.results, allRelevant, k) });

    // FTS5
    const fts5Start = performance.now();
    const fts5Results = fts5Session ? await fts5Query(fts5Session, ann.query, k, verbose) : [];
    const fts5Latency = performance.now() - fts5Start;
    if (fts5Results.length > 0) {
      allResults.push({ mode: "fts5", query: ann.query, repo_name: ann.repo_name, category: ann.category, latency_ms: fts5Latency, results: fts5Results, recall_at_k: recallAtK(fts5Results, allRelevant, k), mrr: mrr(fts5Results, allRelevant), ndcg_at_k: ndcgAtK(fts5Results, allRelevant, k) });
      perQueryCQ.push({
        mode: "fts5",
        cq: { candidate_pool_size: fts5Results.length, rerank_pool_size: k, snippet_count: 0, enriched_candidate_count: 0, path_only_count: fts5Results.length, unenriched_candidate_count: fts5Results.length, avg_doc_tokens: 0, max_doc_tokens: 0, total_doc_tokens: 0, pre_rerank_recall_at_pool: recallAtK(fts5Results, allRelevant, k), post_rerank_recall_at_k: recallAtK(fts5Results, allRelevant, k), lost_relevant_after_rerank: [], context_exhausted: null, reranker_skipped_reason: null },
        recall_at_k: recallAtK(fts5Results, allRelevant, k), hold_out: ann.hold_out === true, intent: ann.category || "unknown", harness_lat_ms: fts5Latency,
      });
    } else { emptyCounts["fts5"] = (emptyCounts["fts5"] || 0) + 1; }

    // AFT grep
    const grepStart = performance.now();
    const grepResults = grepSession ? await grepQuery(grepSession, ann.query, k, verbose) : [];
    const grepLatency = performance.now() - grepStart;
    if (grepResults.length > 0) {
      allResults.push({ mode: "aft-grep", query: ann.query, repo_name: ann.repo_name, category: ann.category, latency_ms: grepLatency, results: grepResults, recall_at_k: recallAtK(grepResults, allRelevant, k), mrr: mrr(grepResults, allRelevant), ndcg_at_k: ndcgAtK(grepResults, allRelevant, k) });
      perQueryCQ.push({
        mode: "aft-grep",
        cq: { candidate_pool_size: grepResults.length, rerank_pool_size: k, snippet_count: 0, enriched_candidate_count: 0, path_only_count: grepResults.length, unenriched_candidate_count: grepResults.length, avg_doc_tokens: 0, max_doc_tokens: 0, total_doc_tokens: 0, pre_rerank_recall_at_pool: recallAtK(grepResults, allRelevant, k), post_rerank_recall_at_k: recallAtK(grepResults, allRelevant, k), lost_relevant_after_rerank: [], context_exhausted: null, reranker_skipped_reason: null },
        recall_at_k: recallAtK(grepResults, allRelevant, k), hold_out: ann.hold_out === true, intent: ann.category || "unknown", harness_lat_ms: grepLatency,
      });
    } else { emptyCounts["aft-grep"] = (emptyCounts["aft-grep"] || 0) + 1; }

    // Semantic backends
    for (const [be, session] of Object.entries(semSessions)) {
      if (!session) { emptyCounts[`semantic-${be}`] = (emptyCounts[`semantic-${be}`] || 0) + 1; continue; }
      const modeName = be === "model2vec" ? "semantic-m2v" : be === "fastembed" ? "semantic-fe" : "semantic-api";

      // Base pass
      const semStart = performance.now();
      const semResults = await semanticQuery(session, ann.query, k, be, verbose);
      const semLatency = performance.now() - semStart;
      if (semResults.length > 0) {
        allResults.push({ mode: modeName, query: ann.query, repo_name: ann.repo_name, category: ann.category, latency_ms: semLatency, results: semResults, recall_at_k: recallAtK(semResults, allRelevant, k), mrr: mrr(semResults, allRelevant), ndcg_at_k: ndcgAtK(semResults, allRelevant, k) });
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
        });
      } else { emptyCounts[modeName] = (emptyCounts[modeName] || 0) + 1; }

      // Hybrid: FTS5 + semantic RRF (FTS5 is optional — hybrid works with semantic-only)
      if (semResults.length > 0) {
        const hybridResults = fts5Results.length > 0
          ? rrfFusion(fts5Results, semResults, k)
          : semResults; // No FTS5 results — use semantic-only as hybrid
        const hybridRecall = recallAtK(hybridResults, allRelevant, k);
        allResults.push({ mode: `hybrid-${modeName.replace("semantic-", "")}`, query: ann.query, repo_name: ann.repo_name, category: ann.category, latency_ms: fts5Latency + semLatency, results: hybridResults, recall_at_k: hybridRecall, mrr: mrr(hybridResults, allRelevant), ndcg_at_k: ndcgAtK(hybridResults, allRelevant, k) });
        const hybridSnippets = hybridResults.filter((r) => (r.snippet || "").length > 10).length;
        const hybridTokenCounts = hybridResults.map((r) => approxTokens(r.snippet || ""));
        perQueryCQ.push({
          mode: `hybrid-${modeName.replace("semantic-", "")}`,
          cq: { candidate_pool_size: hybridResults.length, rerank_pool_size: k, snippet_count: hybridSnippets, enriched_candidate_count: hybridSnippets, path_only_count: hybridResults.length - hybridSnippets, unenriched_candidate_count: hybridResults.length - hybridSnippets, avg_doc_tokens: hybridTokenCounts.length > 0 ? Math.round(hybridTokenCounts.reduce((a, b) => a + b, 0) / hybridTokenCounts.length) : 0, max_doc_tokens: hybridTokenCounts.length > 0 ? Math.max(...hybridTokenCounts) : 0, total_doc_tokens: hybridTokenCounts.reduce((a, b) => a + b, 0), pre_rerank_recall_at_pool: hybridRecall, post_rerank_recall_at_k: hybridRecall, lost_relevant_after_rerank: [], context_exhausted: null, reranker_skipped_reason: null },
          recall_at_k: hybridRecall, hold_out: ann.hold_out === true, intent: ann.category || "unknown", harness_lat_ms: fts5Latency + semLatency,
        });
      }
    }

    // Rerank pass (for each semantic backend)
    if (doRerank) {
      for (const [be, session] of Object.entries(semSessions)) {
        if (!session) continue;
        const modeName = be === "model2vec" ? "semantic-m2v" : be === "fastembed" ? "semantic-fe" : "semantic-api";
        const semResults = await semanticQuery(session, ann.query, k * oversample, be, verbose);
        if (semResults.length === 0) continue;

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

        allResults.push({ mode: `${modeName}+rerank`, query: ann.query, repo_name: ann.repo_name, category: ann.category, latency_ms: rerankLat, results: reranked, recall_at_k: postRecall, mrr: postMrr, ndcg_at_k: postNdcg });

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
          harness_lat_ms: rerankLat,
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
    const lexicalSessions: Record<string, AftSession | null> = {};
    let lexCurrentRepo = "";

    for (const lq of LEXICAL_QUERIES) {
      for (const repoName of lq.repos) {
        if (!availableRepos.has(repoName)) continue;
        const repoDir = join(resolve(cacheDir), repoName);

        // Init sessions when repo changes
        if (repoName !== lexCurrentRepo) {
          for (const s of Object.values(lexicalSessions)) s?.close();
          for (const k of Object.keys(lexicalSessions)) delete lexicalSessions[k];
          lexCurrentRepo = repoName;
          console.log(`\n  Initializing lexical sessions for ${repoName}...`);

          // Init grep + fts5 sessions
          lexicalSessions["aft-grep"] = await initGrepSession(bin, repoDir, verbose);
          lexicalSessions["fts5"] = await initFts5Session(bin, repoDir, verbose);

          // Init semantic sessions for lexical queries
          for (const be of backends) {
            const storageDir = join(repoDir, `.aft-bench-${be}-lex`);
            const beModel = be === "fastembed" ? "all-MiniLM-L6-v2" : be === "semantic-api" ? apiModel : semanticModel;
            lexicalSessions[be] = await initSemanticSession(bin, repoDir, beModel, be, verbose, storageDir, queryPrompt);
          }
        }

        const relevant = [lq.query]; // For identifier queries, the query itself is the relevant "path"
        // Use checked-in canon relevance as ground truth, NOT runtime rg results
        // Load canon relevance if available, otherwise fall back to empty (skip scoring)
        const canonExact = loadCanonSuite(resolve("benchmarks/semble/canon"), "identifier_exact");
        const canonPrefix = loadCanonSuite(resolve("benchmarks/semble/canon"), "identifier_prefix");
        const canonEntry = [...(canonExact?.queries || []), ...(canonPrefix?.queries || [])].find((q) => q.query === lq.query && q.repo_name === repoName);
        const allRelevant = canonEntry ? canonEntry.relevant.map((r) => r.path).filter(Boolean) : [];
        if (allRelevant.length === 0) {
          // No canon relevance for this query — skip scoring (rg is contestant, not oracle)
          if (verbose) console.log(`    SKIP ${lq.query}: no canon relevance defined`);
          continue;
        }

        // Ripgrep (baseline contestant, NOT oracle)
        const rg = rgSearch(lq.query, repoDir, null, k);
        allResults.push({ mode: "lexical (rg)", query: lq.query, repo_name: repoName, category: "identifier", latency_ms: rg.latency_ms, results: rg.results, recall_at_k: recallAtK(rg.results, allRelevant, k), mrr: mrr(rg.results, allRelevant), ndcg_at_k: ndcgAtK(rg.results, allRelevant, k) });

        // AFT grep
        const grepS = lexicalSessions["aft-grep"];
        if (grepS) {
          const gs = performance.now();
          const gr = await grepQuery(grepS, lq.query, k, verbose);
          allResults.push({ mode: "aft-grep", query: lq.query, repo_name: repoName, category: "identifier", latency_ms: performance.now() - gs, results: gr, recall_at_k: recallAtK(gr, allRelevant, k), mrr: mrr(gr, allRelevant), ndcg_at_k: ndcgAtK(gr, allRelevant, k) });
        }

        // FTS5
        const f5S = lexicalSessions["fts5"];
        if (f5S) {
          const fs = performance.now();
          const f5r = await fts5Query(f5S, lq.query, k, verbose);
          if (f5r.length > 0) allResults.push({ mode: "fts5", query: lq.query, repo_name: repoName, category: "identifier", latency_ms: performance.now() - fs, results: f5r, recall_at_k: recallAtK(f5r, allRelevant, k), mrr: mrr(f5r, allRelevant), ndcg_at_k: ndcgAtK(f5r, allRelevant, k) });
        }

        // Semantic backends
        for (const [be, session] of Object.entries(lexicalSessions)) {
          if (!session || be === "aft-grep" || be === "fts5") continue;
          const modeName = be === "model2vec" ? "semantic-m2v" : be === "fastembed" ? "semantic-fe" : "semantic-api";
          const ss = performance.now();
          const sr = await semanticQuery(session, lq.query, k, be, verbose);
          if (sr.length > 0) allResults.push({ mode: modeName, query: lq.query, repo_name: repoName, category: "identifier", latency_ms: performance.now() - ss, results: sr, recall_at_k: recallAtK(sr, allRelevant, k), mrr: mrr(sr, allRelevant), ndcg_at_k: ndcgAtK(sr, allRelevant, k) });
        }
      }
    }
    // Close lexical sessions
    for (const s of Object.values(lexicalSessions)) s?.close();
  }

  // Aggregate results
  const byMode = new Map<string, ModeResult[]>();
  for (const r of allResults) {
    if (!byMode.has(r.mode)) byMode.set(r.mode, []);
    byMode.get(r.mode)!.push(r);
  }

  const totalSemantic = allAnnotations.length;
  const semanticAgg: AggregateMode[] = [];
  const lexicalAgg: AggregateMode[] = [];

  for (const [mode, rows] of byMode) {
    const agg = aggregateMetrics(rows, totalSemantic);
    // Classify by query category: NL queries → semantic table, identifier queries → lexical table
    const hasIdentifier = rows.some((r) => r.category === "identifier");
    const hasSemantic = rows.some((r) => r.category !== "identifier");
    if (hasIdentifier && !hasSemantic) {
      lexicalAgg.push(agg);
    } else if (hasSemantic && !hasIdentifier) {
      semanticAgg.push(agg);
    } else if (hasIdentifier && hasSemantic) {
      // Mixed: show in both tables
      semanticAgg.push(agg);
      lexicalAgg.push(agg);
    } else {
      semanticAgg.push(agg);
    }
  }

  // Sort by recall descending
  semanticAgg.sort((a, b) => b.recall - a.recall);
  lexicalAgg.sort((a, b) => b.recall - a.recall);

  // Compute rerank metrics
  const rerankAgg: Record<string, RerankMetrics> = {};
  for (const mode of Object.keys(rerankResults)) {
    const baseMode = mode.replace("+rerank", "");
    const baseRows = byMode.get(baseMode) || [];
    const rerankRows = byMode.get(mode) || [];
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
  if (semanticAgg.length > 0) printTable(`SEMANTIC SEARCH (NL queries, k=${k})`, semanticAgg, Object.keys(rerankAgg).length > 0 ? rerankAgg : undefined);
  if (lexicalAgg.length > 0) printTable(`LEXICAL SEARCH (identifier queries, k=${k})`, lexicalAgg);

  // Compute context quality per mode
  const contextQualityByMode: Record<string, ContextQuality> = {};

  for (const mode of Object.keys(byMode)) {
    const rows = byMode.get(mode) || [];
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
        engineLats.push(c.engine_latency_p95_ms || 0);
        harnessLats.push(c.benchmark_harness_latency_p95_ms || 0);
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
        enriched_candidate_count: totalEnriched,
        path_only_count: totalPathOnly,
        unenriched_candidate_count: totalPathOnly,
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
        const isHoldout = (row as any).hold_out === true;
        if (isHoldout) holdoutRecalls.push(row.recall_at_k);
        else tuningRecalls.push(row.recall_at_k);
      }

      const rerankPoolSize = rows.length > 0 ? Math.max(rows[0].results?.length || 0, k) : k;
      const tuningRecall10 = tuningRecalls.length > 0
        ? tuningRecalls.reduce((s, v) => s + v, 0) / tuningRecalls.length : 0;
      const holdoutRecall10 = holdoutRecalls.length > 0
        ? holdoutRecalls.reduce((s, v) => s + v, 0) / holdoutRecalls.length : 0;
      const sortedLats = [...allLatencies].sort((a, b) => a - b);

      contextQualityByMode[mode] = {
        candidate_pool_size: rows.length,
        rerank_pool_size: rerankPoolSize,
        snippet_count: totalSnippets,
        enriched_candidate_count: totalSnippets,
        path_only_count: totalPathOnly,
        unenriched_candidate_count: totalPathOnly,
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
  const report = {
    timestamp: new Date().toISOString(),
    k,
    binary: binaryPath,
    backends,
    rerank: doRerank ? { model: rerankModel, url: rerankUrl } : null,
    rerank_context: rerankContext,
    results: allResults,
    aggregate: Object.fromEntries(semanticAgg.map((a) => [a.mode, a])),
    lexical_aggregate: Object.fromEntries(lexicalAgg.map((a) => [a.mode, a])),
    rerank_metrics: rerankAgg,
    context_quality: contextQualityByMode,
    empty_counts: emptyCounts,
  };
  writeFileSync(resolve(outputFile), JSON.stringify(report, null, 2) + "\n");

  // Close sessions
  for (const s of Object.values(semSessions)) s?.close();
  fts5Session?.close();
  grepSession?.close();

  // Empty summary
  const emptyParts = Object.entries(emptyCounts).filter(([, v]) => v > 0).map(([k, v]) => `${k}=${v}`);
  if (emptyParts.length > 0) console.log(`\n  ⚠ Empty results: ${emptyParts.join(" ")}`);
  console.log(`\n  Report saved to ${outputFile}`);
}

main();
