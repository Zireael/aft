#!/usr/bin/env bun
/**
 * Local Semble pilot runner.
 *
 * Runs pilot fixtures against AFT search and produces a comparison report.
 * Compares: lexical (ripgrep), semantic (AFT), hybrid, and reranked modes.
 *
 * Usage:
 *   bun run benchmarks/semble/pilot.ts [options]
 *
 * Options:
 *   --cache-dir <dir>    Repo cache directory (default: .bench-cache)
 *   --input <file>       Fixture file (default: fixtures.json)
 *   --k <n>              Top-k for evaluation (default: 10)
 *   --output <file>      Output report (default: pilot-report.json)
 *   --binary <path>      Path to aft binary (for FTS5/grep/semantic modes)
 *   --model <name>       model2vec model for semantic mode (default: minishlab/potion-code-16M)
 *   --backend <name>     Semantic backend(s): 'both' (default), 'model2vec', 'fastembed', or 'skip'
 *   --verbose, -v        Show per-query debug output
 */

import { readFileSync, writeFileSync, existsSync, statSync } from "fs";
import { join, resolve } from "path";
import { execSync } from "child_process";
import { aftNdjson, AftSession } from "./aft-ndjson";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface Fixture {
  schema_version: number;
  source: { name: string };
  repos: Array<{
    name: string;
    language: string;
    benchmark_root: string | null;
  }>;
  annotations: Array<{
    query: string;
    relevant: Array<{ path: string; start_line?: number; end_line?: number }>;
    secondary: Array<{ path: string; start_line?: number; end_line?: number }>;
    category: string;
    repo_name: string;
  }>;
}

interface SearchResult {
  file: string;
  line?: number;
  score?: number;
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

interface PilotReport {
  timestamp: string;
  fixture_source: string;
  k: number;
  results: ModeResult[];
  aggregate: Record<
    string,
    {
      mean_recall: number;
      mean_mrr: number;
      mean_ndcg: number;
      mean_latency_ms: number;
      query_count: number;
    }
  >;
  by_category: Record<
    string,
    Record<
      string,
      {
        mean_recall: number;
        mean_mrr: number;
        mean_ndcg: number;
      }
    >
  >;
}

// ---------------------------------------------------------------------------
// Scoring helpers
// ---------------------------------------------------------------------------

function normalizePath(p: string): string {
  return p.replace(/\\/g, "/").replace(/^\/\?\//, "").replace(/^\.\//, "");
}

function recallAtK(
  retrieved: SearchResult[],
  relevant: string[],
  k: number
): number {
  if (!retrieved || relevant.length === 0) return 0;
  const rPaths = new Set(retrieved.slice(0, k).map((r) => normalizePath(r.file)));
  let hits = 0;
  for (const r of relevant) {
    const nr = normalizePath(r);
    for (const rp of rPaths) {
      if (rp.endsWith(nr) || nr.endsWith(rp)) {
        hits++;
        break;
      }
    }
  }
  return hits / relevant.length;
}

function mrr(retrieved: SearchResult[], relevant: string[]): number {
  if (!retrieved) return 0;
  for (let i = 0; i < retrieved.length; i++) {
    const rf = normalizePath(retrieved[i].file);
    for (const r of relevant) {
      const nr = normalizePath(r);
      if (rf.endsWith(nr) || nr.endsWith(rf)) return 1 / (i + 1);
    }
  }
  return 0;
}

function ndcgAtK(
  retrieved: SearchResult[],
  relevant: string[],
  k: number
): number {
  if (!retrieved) return 0;
  const relSet = new Set(relevant.map(normalizePath));
  // DCG — track matched relevant paths to avoid double-counting
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
  // Ideal DCG
  const idealHits = Math.min(relSet.size, k);
  let idcg = 0;
  for (let i = 0; i < idealHits; i++) {
    idcg += 1 / Math.log2(i + 2);
  }
  return idcg > 0 ? dcg / idcg : 0;
}

// ---------------------------------------------------------------------------
// Ripgrep (lexical) mode
// ---------------------------------------------------------------------------

function rgSearch(
  query: string,
  searchDir: string,
  benchmarkRoot: string | null,
  k: number
): { results: SearchResult[]; latency_ms: number } {
  const targetDir = benchmarkRoot ? join(searchDir, benchmarkRoot) : searchDir;
  const start = performance.now();
  let results: SearchResult[] = [];

  try {
    const output = execSync(
      `rg -n --no-heading --max-count ${k * 2} "${query.replace(/"/g, '\\"')}" .`,
      { cwd: targetDir, encoding: "utf-8", stdio: "pipe", timeout: 10000 }
    );
    const lines = output.trim().split("\n").filter(Boolean);
    results = lines.slice(0, k).map((line) => {
      const ci = line.indexOf(":");
      const file = line.substring(0, ci);
      const rest = line.substring(ci + 1);
      const ci2 = rest.indexOf(":");
      const lineNum = parseInt(rest.substring(0, ci2), 10);
      return { file, line: lineNum };
    });
  } catch {}

  return { results, latency_ms: performance.now() - start };
}

// ---------------------------------------------------------------------------
// FTS5 mode
// ---------------------------------------------------------------------------

async function fts5Search(
  query: string,
  searchDir: string,
  benchmarkRoot: string | null,
  k: number,
  binaryPath: string | null,
  verbose = false
): Promise<{ results: SearchResult[]; latency_ms: number }> {
  const targetDir = benchmarkRoot ? join(searchDir, benchmarkRoot) : searchDir;
  const bin = binaryPath || "aft";
  const start = performance.now();
  let results: SearchResult[] = [];

  try {
    const commands: Record<string, unknown>[] = [
      {
        id: "cfg-fts5",
        command: "configure",
        harness: "opencode",
        project_root: targetDir,
        storage_dir: join(targetDir, ".aft-bench"),
        fts5: { enabled: true },
      },
      {
        id: "idx-fts5",
        command: "fts5_index",
        action: "update",
      },
      {
        id: "search-fts5",
        command: "fts5_search",
        query,
        scope: "all",
        top_k: k,
      },
    ];

    const responses = await aftNdjson(bin, commands, 60000);
    if (verbose) console.log(`    FTS5 responses: ${responses.length}/${commands.length}`);

    for (const parsed of [...responses].reverse()) {
      const items = parsed.results || parsed.matches || parsed.evidence;
      if (verbose) console.log(`    FTS5 [${parsed.id}]: ${items ? `${items.length} items` : `success=${parsed.success} keys=${Object.keys(parsed).join(',')}`}`);
      if (items && Array.isArray(items)) {
        results = (items as any[]).map((r: any) => ({
          file: r.file_path || r.path || r.file || "",
          line: r.start_line || r.line,
          score: r.score,
        }));
        break;
      }
    }
  } catch {}

  return { results, latency_ms: performance.now() - start };
}

// ---------------------------------------------------------------------------
// Semantic search mode (model2vec + built-in ONNX)
// Uses a persistent AFT session per backend so the model loads once and stays
// in memory across all queries on the same repo.
// ---------------------------------------------------------------------------

/** Initialize a semantic session for a repo: configure + wait for index ready. */
async function initSemanticSession(
  bin: string,
  targetDir: string,
  model: string,
  backend: string,
  verbose: boolean
): Promise<AftSession | null> {
  const session = new AftSession(bin);
  try {
    await session.call({
      command: "configure",
      harness: "opencode",
      project_root: targetDir,
      storage_dir: join(targetDir, ".aft-bench"),
      semantic_search: true,
      semantic: { backend, model },
    }, 30000);

    // Poll status until ready (max 180s)
    const readyDeadline = Date.now() + 180_000;
    while (Date.now() < readyDeadline) {
      const status = await session.call({ command: "status" }, 10_000);
      const semStatus = (status as any).semantic_index?.status;
      if (verbose) process.stdout.write(`    SEM-${backend} status: ${semStatus}\r`);
      if (semStatus === "ready" || semStatus === "partial") {
        if (verbose) process.stdout.write(`    SEM-${backend} status: ready     \n`);
        return session;
      }
      if (semStatus === "failed" || semStatus === "disabled") {
        if (verbose) process.stdout.write(`    SEM-${backend} status: ${semStatus}     \n`);
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

/** Run a semantic search query on an already-initialized session. */
async function semanticQuery(
  session: AftSession,
  query: string,
  k: number,
  backend: string,
  verbose: boolean
): Promise<SearchResult[]> {
  try {
    const searchResp = await session.call({
      command: "semantic_search",
      query,
      topK: k,
    }, 30_000);

    const items = (searchResp as any).results;
    if (verbose) console.log(`    SEM-${backend} [search]: ${items ? `${items.length} items` : `success=${searchResp.success}`}`);
    if (items && Array.isArray(items)) {
      return (items as any[]).map((r: any) => ({
        file: r.file || r.file_path || r.path || "",
        line: r.start_line || r.line,
        score: r.score,
      }));
    }
  } catch (e) {
    if (verbose) console.log(`    SEM-${backend} search ERROR: ${e}`);
  }
  return [];
}

// ---------------------------------------------------------------------------
// AFT grep mode (trigram-indexed)
// ---------------------------------------------------------------------------

async function aftGrepSearch(
  query: string,
  searchDir: string,
  benchmarkRoot: string | null,
  k: number,
  binaryPath: string | null,
  verbose = false
): Promise<{ results: SearchResult[]; latency_ms: number }> {
  const targetDir = benchmarkRoot ? join(searchDir, benchmarkRoot) : searchDir;
  const bin = binaryPath || "aft";
  const start = performance.now();
  let results: SearchResult[] = [];

  try {
    const commands: Record<string, unknown>[] = [
      {
        id: "cfg-aft",
        command: "configure",
        harness: "opencode",
        project_root: targetDir,
        storage_dir: join(targetDir, ".aft-bench"),
      },
      {
        id: "search-aft",
        command: "grep",
        pattern: query,
        max_results: k,
      },
    ];

    const responses = await aftNdjson(bin, commands, 60000);
    if (verbose) console.log(`    GREP responses: ${responses.length}/${commands.length}`);

    for (const parsed of [...responses].reverse()) {
      const items = parsed.results || parsed.matches || parsed.evidence;
      if (verbose) console.log(`    GREP [${parsed.id}]: ${items ? `${items.length} items` : `success=${parsed.success} keys=${Object.keys(parsed).join(',')}`}`);
      if (items && Array.isArray(items)) {
        results = (items as any[]).map((r: any) => ({
          file: r.file_path || r.path || r.file || "",
          line: r.start_line || r.line,
          score: r.score,
        }));
        break;
      }
    }
  } catch {}

  return { results, latency_ms: performance.now() - start };
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

async function main() {
  const args = process.argv.slice(2);
  let cacheDir = ".bench-cache";
  let inputFile = "benchmarks/semble/fixtures.json";
  let k = 10;
  let outputFile = "pilot-report.json";
    let binaryPath: string | null = null;
    let verbose = false;
    let semanticModel = "minishlab/potion-code-16M";
    let semanticBackend = "both";

  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--cache-dir":
        cacheDir = args[++i];
        break;
      case "--input":
        inputFile = args[++i];
        break;
      case "--k":
        k = parseInt(args[++i], 10);
        break;
      case "--output":
        outputFile = args[++i];
        break;
      case "--binary":
        binaryPath = args[++i]; break;
      case "--verbose":
      case "-v":
        verbose = true; break;
      case "--model":
        semanticModel = args[++i]; break;
      case "--backend":
        semanticBackend = args[++i]; break;
    }
  }

  const fixture: Fixture = JSON.parse(readFileSync(resolve(inputFile), "utf-8"));
  console.log(
    `Running pilot: ${fixture.annotations.length} queries across ${fixture.repos.length} repos (k=${k})`
  );

  const allResults: ModeResult[] = [];
  let fts5EmptyCount = 0;
  let aftGrepEmptyCount = 0;
  let m2vEmptyCount = 0;
  let feEmptyCount = 0;

  // Semantic sessions — created once per repo, reused across queries
  const bin = binaryPath || "aft";
  let currentRepoName = "";
  const semSessions: Record<string, AftSession | null> = {};

  // Verify binary exists (pilot always runs fts5 + aft-grep modes)
  if (binaryPath) {
    try {
      statSync(binaryPath);
    } catch {
      console.error(`\nERROR: AFT binary not found at: ${binaryPath}`);
      console.error(`Pass --binary <path> to the aft binary, or build with:`);
      console.error(`  cargo build --release --features semantic-fts5`);
      process.exit(1);
    }
  }

  for (const ann of fixture.annotations) {
    const repo = fixture.repos.find((r) => r.name === ann.repo_name);
    if (!repo) continue;

    const repoDir = join(resolve(cacheDir), repo.name);
    if (!existsSync(repoDir)) continue;

    // When repo changes, close old semantic sessions and init new ones
    if (ann.repo_name !== currentRepoName) {
      // Close old sessions
      for (const s of Object.values(semSessions)) s?.close();
      for (const k of Object.keys(semSessions)) delete semSessions[k];

      currentRepoName = ann.repo_name;
      const targetDir = repo.benchmark_root ? join(repoDir, repo.benchmark_root) : repoDir;
      console.log(`\n  Initializing semantic sessions for ${ann.repo_name}...`);

      // Init sessions for each requested backend
      const backends = semanticBackend === "skip" ? []
        : semanticBackend === "both" ? ["model2vec", "fastembed"]
        : [semanticBackend];
      for (const be of backends) {
        semSessions[be] = await initSemanticSession(bin, targetDir, semanticModel, be, verbose);
      }
    }

    const allRelevant = [
      ...ann.relevant.map((r) => r.path),
      ...ann.secondary.map((r) => r.path),
    ];

    // Lexical mode (ripgrep)
    const { results: rgResults, latency_ms: rgLatency } = rgSearch(
      ann.query,
      repoDir,
      repo.benchmark_root,
      k
    );

    allResults.push({
      mode: "lexical",
      query: ann.query,
      repo_name: ann.repo_name,
      category: ann.category,
      latency_ms: rgLatency,
      results: rgResults,
      recall_at_k: recallAtK(rgResults, allRelevant, k),
      mrr: mrr(rgResults, allRelevant),
      ndcg_at_k: ndcgAtK(rgResults, allRelevant, k),
    });

    // FTS5 mode
    const { results: fts5Results, latency_ms: fts5Latency } = await fts5Search(
      ann.query,
      repoDir,
      repo.benchmark_root,
      k,
      binaryPath,
      verbose
    );

    if (fts5Results.length > 0) {
      allResults.push({
        mode: "fts5",
        query: ann.query,
        repo_name: ann.repo_name,
        category: ann.category,
        latency_ms: fts5Latency,
        results: fts5Results,
        recall_at_k: recallAtK(fts5Results, allRelevant, k),
        mrr: mrr(fts5Results, allRelevant),
        ndcg_at_k: ndcgAtK(fts5Results, allRelevant, k),
      });
    } else {
      fts5EmptyCount++;
      if (verbose) console.log(`  FTS5 EMPTY: "${ann.query}" [${ann.repo_name}]`);
    }

    // AFT grep mode (trigram-indexed)
    const { results: aftResults, latency_ms: aftLatency } = await aftGrepSearch(
      ann.query,
      repoDir,
      repo.benchmark_root,
      k,
      binaryPath,
      verbose
    );

    if (aftResults.length > 0) {
      allResults.push({
        mode: "aft-grep",
        query: ann.query,
        repo_name: ann.repo_name,
        category: ann.category,
        latency_ms: aftLatency,
        results: aftResults,
        recall_at_k: recallAtK(aftResults, allRelevant, k),
        mrr: mrr(aftResults, allRelevant),
        ndcg_at_k: ndcgAtK(aftResults, allRelevant, k),
      });
    } else {
      aftGrepEmptyCount++;
      if (verbose) console.log(`  GREP EMPTY: "${ann.query}" [${ann.repo_name}]`);
    }

    // Semantic search — query on persistent sessions
    for (const [backend, session] of Object.entries(semSessions)) {
      if (!session) continue;
      const modeName = backend === "model2vec" ? "semantic-m2v" : "semantic-fe";
      const start = performance.now();
      const semResults = await semanticQuery(session, ann.query, k, backend, verbose);
      const semLatency = performance.now() - start;

      if (semResults.length > 0) {
        allResults.push({
          mode: modeName,
          query: ann.query,
          repo_name: ann.repo_name,
          category: ann.category,
          latency_ms: semLatency,
          results: semResults,
          recall_at_k: recallAtK(semResults, allRelevant, k),
          mrr: mrr(semResults, allRelevant),
          ndcg_at_k: ndcgAtK(semResults, allRelevant, k),
        });
      } else {
        if (modeName === "semantic-m2v") m2vEmptyCount++;
        else feEmptyCount++;
        if (verbose) console.log(`  ${modeName.toUpperCase()} EMPTY: "${ann.query}" [${ann.repo_name}]`);
      }
    }
  }

  // Aggregate by mode
  const byMode: Record<string, { recalls: number[]; mrrs: number[]; ndcgs: number[]; latencies: number[] }> = {};
  for (const r of allResults) {
    if (!byMode[r.mode]) byMode[r.mode] = { recalls: [], mrrs: [], ndcgs: [], latencies: [] };
    byMode[r.mode].recalls.push(r.recall_at_k);
    byMode[r.mode].mrrs.push(r.mrr);
    byMode[r.mode].ndcgs.push(r.ndcg_at_k);
    byMode[r.mode].latencies.push(r.latency_ms);
  }

  const aggregate: Record<string, any> = {};
  for (const [mode, data] of Object.entries(byMode)) {
    const n = data.recalls.length;
    aggregate[mode] = {
      mean_recall: data.recalls.reduce((s, v) => s + v, 0) / n,
      mean_mrr: data.mrrs.reduce((s, v) => s + v, 0) / n,
      mean_ndcg: data.ndcgs.reduce((s, v) => s + v, 0) / n,
      mean_latency_ms: data.latencies.reduce((s, v) => s + v, 0) / n,
      query_count: n,
    };
  }

  // By category
  const byCategory: Record<string, Record<string, { recalls: number[]; mrrs: number[]; ndcgs: number[] }>> = {};
  for (const r of allResults) {
    if (!byCategory[r.category]) byCategory[r.category] = {};
    if (!byCategory[r.category][r.mode])
      byCategory[r.category][r.mode] = { recalls: [], mrrs: [], ndcgs: [] };
    byCategory[r.category][r.mode].recalls.push(r.recall_at_k);
    byCategory[r.category][r.mode].mrrs.push(r.mrr);
    byCategory[r.category][r.mode].ndcgs.push(r.ndcg_at_k);
  }

  const byCategoryAgg: Record<string, Record<string, any>> = {};
  for (const [cat, modes] of Object.entries(byCategory)) {
    byCategoryAgg[cat] = {};
    for (const [mode, data] of Object.entries(modes)) {
      const n = data.recalls.length;
      byCategoryAgg[cat][mode] = {
        mean_recall: data.recalls.reduce((s, v) => s + v, 0) / n,
        mean_mrr: data.mrrs.reduce((s, v) => s + v, 0) / n,
        mean_ndcg: data.ndcgs.reduce((s, v) => s + v, 0) / n,
      };
    }
  }

  const report: PilotReport = {
    timestamp: new Date().toISOString(),
    fixture_source: fixture.source.name,
    k,
    results: allResults,
    aggregate,
    by_category: byCategoryAgg,
  };

  writeFileSync(resolve(outputFile), JSON.stringify(report, null, 2) + "\n");

  // Close all semantic sessions
  for (const s of Object.values(semSessions)) s?.close();

  console.log(`\n=== Pilot Report ===`);
  const modes = ["lexical", "fts5", "semantic-m2v", "semantic-fe", "aft-grep"];
  for (const mode of modes) {
    const data = aggregate[mode];
    if (data) {
      console.log(
        `  ${mode}: recall=${(data.mean_recall * 100).toFixed(1)}% mrr=${data.mean_mrr.toFixed(3)} ndcg=${data.mean_ndcg.toFixed(3)} latency=${data.mean_latency_ms.toFixed(1)}ms (${data.query_count} queries)`
      );
    } else {
      console.log(`  ${mode}: NO RESULTS`);
    }
  }

  // Report empty mode counts
  const emptyParts: string[] = [];
  if (fts5EmptyCount > 0) emptyParts.push(`fts5=${fts5EmptyCount}/${fixture.annotations.length}`);
  if (m2vEmptyCount > 0) emptyParts.push(`semantic-m2v=${m2vEmptyCount}/${fixture.annotations.length}`);
  if (feEmptyCount > 0) emptyParts.push(`semantic-fe=${feEmptyCount}/${fixture.annotations.length}`);
  if (aftGrepEmptyCount > 0) emptyParts.push(`aft-grep=${aftGrepEmptyCount}/${fixture.annotations.length}`);
  if (emptyParts.length > 0) {
    console.log(`\n  ⚠ Empty results: ${emptyParts.join(' ')}`);
  }
}

main();
