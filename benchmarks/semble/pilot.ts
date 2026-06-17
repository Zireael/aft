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

async function applyRerank(
  query: string,
  results: SearchResult[],
  k: number,
  repoDir: string,
  verbose: boolean,
): Promise<{ results: SearchResult[]; latency_ms: number }> {
  const candidates = results.slice(0, k * 5);
  if (candidates.length <= 1) return { results: candidates, latency_ms: 0 };

  const readStart = performance.now();
  // Read file content snippets for reranker
  const documents = candidates.map((r) => {
    const rawFile = r.file || "";
    // Strip Windows UNC prefix
    const normalized = rawFile.replace(/^\\\\\?\\/, "");
    // Determine if path is absolute
    const maybeAbsolute = /^[A-Za-z]:[\\/]/.test(normalized) || normalized.startsWith("/");
    const resolved = maybeAbsolute ? normalized : join(repoDir, normalized);
    try {
      const content = readFileSync(resolved, "utf-8");
      return content.length > 20_000 ? content.slice(0, 20_000) : content;
    } catch {
      // Fallback: try raw path
      try {
        const content = readFileSync(rawFile, "utf-8");
        return content.length > 20_000 ? content.slice(0, 20_000) : content;
      } catch {
        return normalized; // Last resort: return path string
      }
    }
  });
  const readMs = performance.now() - readStart;
  // Log if documents are short (likely path strings, not content)
  if (verbose && documents.some((d) => d.length < 200)) {
    console.log(`    RERANK WARNING: ${documents.filter((d) => d.length < 200).length}/${documents.length} documents are short (<200 chars) — may be path strings, not file content`);
  }

  const start = performance.now();
  try {
    const resp = await fetch(RERANK_URL, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ model: RERANK_MODEL, query, documents, top_n: Math.min(k, candidates.length) }),
      signal: AbortSignal.timeout(30_000),
    });

    if (!resp.ok) {
      if (verbose) console.log(`    RERANK HTTP ${resp.status}`);
      return { results: candidates, latency_ms: performance.now() - start };
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
    if (ranked.length === 0) return { results: candidates, latency_ms: readMs + (performance.now() - start) };

    const rankedKeys = new Set(ranked.map((r) => normalizePath(r.file)));
    const tail = candidates.filter((r) => !rankedKeys.has(normalizePath(r.file)));
    return { results: [...ranked, ...tail], latency_ms: readMs + (performance.now() - start) };
  } catch (e) {
    if (verbose) console.log(`    RERANK ERROR: ${e}`);
    return { results: candidates, latency_ms: readMs + (performance.now() - start) };
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
  verbose: boolean, storageDir?: string,
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
      config.semantic = { backend: "openai_compatible", base_url: url, model: modelName };
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
      return items.map((r: any) => ({ file: r.file || r.file_path || r.path || "", line: r.start_line || r.line, score: r.score }));
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
  m2vModel?: string; feModel?: string;
}) {
  const W = "\x1b[1;37m", D = "\x1b[0;90m", N = "\x1b[0m";
  const bar = "═".repeat(65);
  console.log(`\n${W}${bar}${N}`);
  console.log(`${W}  AFT Search Benchmark${N}`);
  console.log(`${W}${bar}${N}`);
  console.log(`${D}  k=${opts.k}  repos=${opts.repos.join(", ")}  queries=${opts.semanticCount} NL + ${opts.lexicalCount} identifier${N}`);

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
  let rerankUrl = "http://127.0.0.1:8090/v1/rerank";
  let includeLexical = true;
  let interactive = false;

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
      case "--include-lexical": includeLexical = args[++i] !== "false"; break;
      case "--interactive": interactive = true; break;
      case "--help": case "-h":
        console.log("Usage: bun run benchmarks/semble/pilot.ts --binary <path> [options]");
        console.log("  --k, --backend, --rerank, --semantic-api-url, --verbose");
        process.exit(0);
    }
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
        semSessions[be] = await initSemanticSession(bin, targetDir, beModel, be, verbose, storageDir);
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
    } else { emptyCounts["fts5"] = (emptyCounts["fts5"] || 0) + 1; }

    // AFT grep
    const grepStart = performance.now();
    const grepResults = grepSession ? await grepQuery(grepSession, ann.query, k, verbose) : [];
    const grepLatency = performance.now() - grepStart;
    if (grepResults.length > 0) {
      allResults.push({ mode: "aft-grep", query: ann.query, repo_name: ann.repo_name, category: ann.category, latency_ms: grepLatency, results: grepResults, recall_at_k: recallAtK(grepResults, allRelevant, k), mrr: mrr(grepResults, allRelevant), ndcg_at_k: ndcgAtK(grepResults, allRelevant, k) });
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
      } else { emptyCounts[modeName] = (emptyCounts[modeName] || 0) + 1; }

      // Hybrid: FTS5 + semantic RRF (FTS5 is optional — hybrid works with semantic-only)
      if (semResults.length > 0) {
        const hybridResults = fts5Results.length > 0
          ? rrfFusion(fts5Results, semResults, k)
          : semResults; // No FTS5 results — use semantic-only as hybrid
        allResults.push({ mode: `hybrid-${modeName.replace("semantic-", "")}`, query: ann.query, repo_name: ann.repo_name, category: ann.category, latency_ms: fts5Latency + semLatency, results: hybridResults, recall_at_k: recallAtK(hybridResults, allRelevant, k), mrr: mrr(hybridResults, allRelevant), ndcg_at_k: ndcgAtK(hybridResults, allRelevant, k) });
      }
    }

    // Rerank pass (for each semantic backend)
    if (doRerank) {
      for (const [be, session] of Object.entries(semSessions)) {
        if (!session) continue;
        const modeName = be === "model2vec" ? "semantic-m2v" : be === "fastembed" ? "semantic-fe" : "semantic-api";
        const semResults = await semanticQuery(session, ann.query, k * 5, be, verbose);
        if (semResults.length === 0) continue;

        const preRecall = recallAtK(semResults, allRelevant, k * 5);
        const { results: reranked, latency_ms: rerankLat } = await applyRerank(ann.query, semResults, k, repoDir, verbose);
        const postRecall = recallAtK(reranked, allRelevant, k);
        const postMrr = mrr(reranked, allRelevant);
        const postNdcg = ndcgAtK(reranked, allRelevant, k);

        allResults.push({ mode: `${modeName}+rerank`, query: ann.query, repo_name: ann.repo_name, category: ann.category, latency_ms: rerankLat, results: reranked, recall_at_k: postRecall, mrr: postMrr, ndcg_at_k: postNdcg });

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
            lexicalSessions[be] = await initSemanticSession(bin, repoDir, beModel, be, verbose, storageDir);
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

  // Write JSON report
  const report = {
    timestamp: new Date().toISOString(),
    k,
    binary: binaryPath,
    backends,
    rerank: doRerank ? { model: rerankModel, url: rerankUrl } : null,
    results: allResults,
    aggregate: Object.fromEntries(semanticAgg.map((a) => [a.mode, a])),
    lexical_aggregate: Object.fromEntries(lexicalAgg.map((a) => [a.mode, a])),
    rerank_metrics: rerankAgg,
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
