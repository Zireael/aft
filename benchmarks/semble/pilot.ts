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
 */

import { readFileSync, writeFileSync, existsSync, statSync } from "fs";
import { join, resolve } from "path";
import { execSync } from "child_process";
import { aftNdjson } from "./aft-ndjson";

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
  // DCG
  let dcg = 0;
  for (let i = 0; i < Math.min(k, retrieved.length); i++) {
    const rf = normalizePath(retrieved[i].file);
    const isRelevant = [...relSet].some(
      (r) => rf.endsWith(r) || r.endsWith(rf)
    );
    if (isRelevant) dcg += 1 / Math.log2(i + 2);
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
  binaryPath: string | null
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

    for (const parsed of [...responses].reverse()) {
      const items = parsed.results || parsed.matches;
      if (items && Array.isArray(items)) {
        results = (items as any[]).map((r: any) => ({
          file: r.file_path || r.path || "",
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
// AFT grep mode (trigram-indexed)
// ---------------------------------------------------------------------------

async function aftGrepSearch(
  query: string,
  searchDir: string,
  benchmarkRoot: string | null,
  k: number,
  binaryPath: string | null
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

    for (const parsed of [...responses].reverse()) {
      const items = parsed.results || parsed.matches;
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
        binaryPath = args[++i];
        break;
    }
  }

  const fixture: Fixture = JSON.parse(readFileSync(resolve(inputFile), "utf-8"));
  console.log(
    `Running pilot: ${fixture.annotations.length} queries across ${fixture.repos.length} repos (k=${k})`
  );

  const allResults: ModeResult[] = [];

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
      binaryPath
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
    }

    // AFT grep mode (trigram-indexed)
    const { results: aftResults, latency_ms: aftLatency } = await aftGrepSearch(
      ann.query,
      repoDir,
      repo.benchmark_root,
      k,
      binaryPath
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

  console.log(`\n=== Pilot Report ===`);
  for (const [mode, data] of Object.entries(aggregate)) {
    console.log(
      `  ${mode}: recall=${(data.mean_recall * 100).toFixed(1)}% mrr=${data.mean_mrr.toFixed(3)} ndcg=${data.mean_ndcg.toFixed(3)} latency=${data.mean_latency_ms.toFixed(1)}ms`
    );
  }
}

main();
