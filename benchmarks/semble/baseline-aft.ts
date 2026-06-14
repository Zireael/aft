#!/usr/bin/env bun
/**
 * AFT legacy search baseline for Semble benchmarks.
 *
 * Runs AFT grep (trigram-indexed) queries against cloned repos and measures
 * recall@k and latency. Compares against ripgrep and FTS5 baselines.
 *
 * Usage:
 *   bun run benchmarks/semble/baseline-aft.ts [options]
 *
 * Options:
 *   --pilot              Use pilot fixture set
 *   --cache-dir <dir>    Repo cache directory (default: .bench-cache)
 *   --input <file>       Fixture file (default: fixtures.json)
 *   --k <n>              Top-k for recall calculation (default: 10)
 *   --output <file>      Output report (default: baseline-aft-report.json)
 *   --binary <path>      AFT binary path (default: aft)
 *   --mode <mode>        Search mode: grep (default) | semantic | hybrid
 */

import { readFileSync, writeFileSync, existsSync } from "fs";
import { readFileSync, writeFileSync, existsSync, statSync } from "fs";
import { join, resolve } from "path";
import { aftNdjson } from "./aft-ndjson";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface Fixture {
  schema_version: number;
  source: { name: string; upstream: string };
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

interface QueryResult {
  query: string;
  repo_name: string;
  category: string;
  latency_ms: number;
  results: SearchResult[];
  recall_at_k: number;
  mrr: number;
}

interface BaselineReport {
  timestamp: string;
  fixture_source: string;
  backend: string;
  mode: string;
  k: number;
  results: QueryResult[];
  aggregate: {
    mean_recall: number;
    mean_mrr: number;
    mean_latency_ms: number;
    query_count: number;
  };
  by_category: Record<
    string,
    {
      mean_recall: number;
      mean_mrr: number;
      query_count: number;
    }
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
  const rPaths = new Set(
    retrieved.slice(0, k).map((r) => normalizePath(r.file))
  );
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

// ---------------------------------------------------------------------------
// AFT search
// ---------------------------------------------------------------------------

async function aftSearch(
  query: string,
  searchDir: string,
  benchmarkRoot: string | null,
  k: number,
  binaryPath: string,
  mode: string
): Promise<{ results: SearchResult[]; latency_ms: number }> {
  const targetDir = benchmarkRoot ? join(searchDir, benchmarkRoot) : searchDir;
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
        semantic_search: mode !== "grep",
      },
    ];

    if (mode === "grep") {
      commands.push({
        id: "search-aft",
        command: "grep",
        pattern: query,
        max_results: k,
      });
    } else {
      commands.push({
        id: "search-aft",
        command: "semantic_search",
        query,
        top_k: k,
        mode,
      });
    }

    const responses = await aftNdjson(binaryPath, commands, 60000);

    for (const parsed of [...responses].reverse()) {
      // Handle both "results" (semantic_search) and "matches" (grep) response formats
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
  } catch (e) {
    console.error(`  aftSearch error: ${e}`);
  }

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
  let outputFile = "baseline-aft-report.json";
  let binaryPath = "aft";
  let mode = "grep";

  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--pilot":
        inputFile = "benchmarks/semble/fixtures.json";
        break;
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
      case "--mode":
        mode = args[++i];
        break;
    }
  }

  if (!["grep", "semantic", "hybrid"].includes(mode)) {
    console.error(`Invalid mode: ${mode}. Must be grep, semantic, or hybrid.`);
    process.exit(1);
  }

  const fixture: Fixture = JSON.parse(readFileSync(resolve(inputFile), "utf-8"));
  console.log(
    `Running AFT baseline (${mode}): ${fixture.annotations.length} queries across ${fixture.repos.length} repos (k=${k})`
  );

  // Verify binary exists
  try {
    statSync(binaryPath);
  } catch {
    console.error(`\nERROR: AFT binary not found at: ${binaryPath}`);
    console.error(`Pass --binary <path> to the aft binary, or build with:`);
    console.error(`  cargo build --release --features semantic-fts5`);
    process.exit(1);
  }

  const allResults: QueryResult[] = [];

  for (const ann of fixture.annotations) {
    const repo = fixture.repos.find((r) => r.name === ann.repo_name);
    if (!repo) continue;

    const repoDir = join(resolve(cacheDir), repo.name);
    if (!existsSync(repoDir)) continue;

    const allRelevant = [
      ...ann.relevant.map((r) => r.path),
      ...ann.secondary.map((r) => r.path),
    ];

    const { results, latency_ms } = await aftSearch(
      ann.query,
      repoDir,
      repo.benchmark_root,
      k,
      binaryPath,
      mode
    );

    allResults.push({
      query: ann.query,
      repo_name: ann.repo_name,
      category: ann.category,
      latency_ms,
      results,
      recall_at_k: recallAtK(results, allRelevant, k),
      mrr: mrr(results, allRelevant),
    });
  }

  // Aggregate
  const n = allResults.length;
  const aggregate = {
    mean_recall:
      allResults.reduce((s, r) => s + r.recall_at_k, 0) / n,
    mean_mrr: allResults.reduce((s, r) => s + r.mrr, 0) / n,
    mean_latency_ms:
      allResults.reduce((s, r) => s + r.latency_ms, 0) / n,
    query_count: n,
  };

  // By category
  const byCat: Record<
    string,
    { recalls: number[]; mrrs: number[] }
  > = {};
  for (const r of allResults) {
    if (!byCat[r.category]) byCat[r.category] = { recalls: [], mrrs: [] };
    byCat[r.category].recalls.push(r.recall_at_k);
    byCat[r.category].mrrs.push(r.mrr);
  }
  const byCategory: Record<string, any> = {};
  for (const [cat, data] of Object.entries(byCat)) {
    const cn = data.recalls.length;
    byCategory[cat] = {
      mean_recall: data.recalls.reduce((s, v) => s + v, 0) / cn,
      mean_mrr: data.mrrs.reduce((s, v) => s + v, 0) / cn,
      query_count: cn,
    };
  }

  const report: BaselineReport = {
    timestamp: new Date().toISOString(),
    fixture_source: fixture.source.name,
    backend: "aft",
    mode,
    k,
    results: allResults,
    aggregate,
    by_category: byCategory,
  };

  writeFileSync(resolve(outputFile), JSON.stringify(report, null, 2) + "\n");

  console.log(`\n=== AFT Baseline Report (${mode}) ===`);
  console.log(
    `  recall@${k}=${(aggregate.mean_recall * 100).toFixed(1)}% mrr=${aggregate.mean_mrr.toFixed(3)} latency=${aggregate.mean_latency_ms.toFixed(1)}ms`
  );
  for (const [cat, data] of Object.entries(byCategory)) {
    console.log(
      `  ${cat}: recall=${(data.mean_recall * 100).toFixed(1)}% mrr=${data.mean_mrr.toFixed(3)} (${data.query_count} queries)`
    );
  }
}

main();
