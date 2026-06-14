#!/usr/bin/env bun
/**
 * FTS5 baseline for AFT Semble benchmarks.
 *
 * Runs FTS5 queries against cloned repos and measures recall@k and latency.
 * Compares FTS5 against the ripgrep lexical baseline.
 *
 * Usage:
 *   bun run benchmarks/semble/baseline-fts5.ts [options]
 *
 * Options:
 *   --pilot              Use pilot fixture set
 *   --cache-dir <dir>    Repo cache directory (default: .bench-cache)
 *   --input <file>       Fixture file (default: fixtures.json)
 *   --k <n>              Top-k for recall calculation (default: 10)
 *   --output <file>      Output report (default: baseline-fts5-report.json)
 *   --binary <path>      AFT binary path (default: auto-detect)
 */

import { readFileSync, writeFileSync, existsSync } from "fs";
import { join, resolve } from "path";
import { spawnSync } from "child_process";

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

interface Fts5Result {
  file: string;
  line?: number;
  score?: number;
}

interface QueryResult {
  query: string;
  repo_name: string;
  category: string;
  latency_ms: number;
  results: Fts5Result[];
  recall_at_k: number;
  mrr: number;
}

interface BaselineReport {
  timestamp: string;
  fixture_source: string;
  backend: string;
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
  return p.replace(/\\/g, "/").replace(/^\.\//, "");
}

function recallAtK(
  retrieved: Fts5Result[],
  relevant: string[],
  k: number
): number {
  if (relevant.length === 0) return 0;
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

function mrr(retrieved: Fts5Result[], relevant: string[]): number {
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
// FTS5 search
// ---------------------------------------------------------------------------

function fts5Search(
  query: string,
  searchDir: string,
  benchmarkRoot: string | null,
  k: number,
  binaryPath: string
): { results: Fts5Result[]; latency_ms: number } {
  const targetDir = benchmarkRoot ? join(searchDir, benchmarkRoot) : searchDir;
  const start = performance.now();
  let results: Fts5Result[] = [];

  try {
    // Write NDJSON commands to stdin
    const commands = [
      JSON.stringify({
        id: "cfg-fts5",
        command: "configure",
        harness: "opencode",
        project_root: targetDir,
        storage_dir: join(targetDir, ".aft-bench"),
      }),
      JSON.stringify({
        id: "idx-fts5",
        command: "fts5_index",
        action: "update",
      }),
      JSON.stringify({
        id: "search-fts5",
        command: "fts5_search",
        query,
        scope: "all",
        top_k: k,
      }),
    ].join("\n");

    const result = spawnSync(binaryPath, [], {
      input: commands + "\n",
      encoding: "utf-8",
      timeout: 30000,
      stdio: "pipe",
    });

    if (result.stdout) {
      const lines = result.stdout.trim().split("\n").filter(Boolean);
      // Find the search response (last JSON line that has results)
      for (const line of lines.reverse()) {
        try {
          const parsed = JSON.parse(line);
          if (parsed.results && Array.isArray(parsed.results)) {
            results = parsed.results.map((r: any) => ({
              file: r.file_path || r.path || "",
              line: r.start_line || r.line,
              score: r.score,
            }));
            break;
          }
        } catch {}
      }
    }
  } catch {}

  return { results, latency_ms: performance.now() - start };
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

function main() {
  const args = process.argv.slice(2);
  let cacheDir = ".bench-cache";
  let inputFile = "benchmarks/semble/fixtures.json";
  let k = 10;
  let outputFile = "baseline-fts5-report.json";
  let binaryPath = "aft";

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
    }
  }

  const fixture: Fixture = JSON.parse(readFileSync(resolve(inputFile), "utf-8"));
  console.log(
    `Running FTS5 baseline: ${fixture.annotations.length} queries across ${fixture.repos.length} repos (k=${k})`
  );

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

    const { results, latency_ms } = fts5Search(
      ann.query,
      repoDir,
      repo.benchmark_root,
      k,
      binaryPath
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
    backend: "fts5",
    k,
    results: allResults,
    aggregate,
    by_category: byCategory,
  };

  writeFileSync(resolve(outputFile), JSON.stringify(report, null, 2) + "\n");

  console.log(`\n=== FTS5 Baseline Report ===`);
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
