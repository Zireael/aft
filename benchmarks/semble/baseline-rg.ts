#!/usr/bin/env bun
/**
 * Ripgrep lexical-only baseline for AFT Semble benchmarks.
 *
 * Runs ripgrep queries against cloned repos and measures recall@k and latency.
 *
 * Usage:
 *   bun run benchmarks/semble/baseline-rg.ts [options]
 *
 * Options:
 *   --pilot              Use pilot fixture set
 *   --cache-dir <dir>    Repo cache directory (default: .bench-cache)
 *   --input <file>       Fixture file (default: fixtures.json)
 *   --k <n>              Top-k for recall calculation (default: 10)
 *   --output <file>      Output report (default: baseline-rg-report.json)
 */

import { readFileSync, writeFileSync, existsSync } from "fs";
import { join, resolve } from "path";
import { execSync } from "child_process";

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

interface RgResult {
  file: string;
  line: number;
  match: string;
}

interface QueryResult {
  query: string;
  repo_name: string;
  category: string;
  latency_ms: number;
  results: RgResult[];
  recall_at_k: number;
  mrr: number;
}

interface BaselineReport {
  timestamp: string;
  fixture_source: string;
  k: number;
  total_queries: number;
  results: QueryResult[];
  aggregate: {
    mean_recall_at_k: number;
    mean_mrr: number;
    mean_latency_ms: number;
    p50_latency_ms: number;
    p95_latency_ms: number;
    by_category: Record<string, { recall: number; mrr: number; latency: number }>;
  };
}

// ---------------------------------------------------------------------------
// Ripgrep search
// ---------------------------------------------------------------------------

function rgSearch(
  query: string,
  searchDir: string,
  benchmarkRoot: string | null,
  k: number
): { results: RgResult[]; latency_ms: number } {
  const start = performance.now();
  let results: RgResult[] = [];

  // Search within benchmark_root if specified, otherwise search entire repo
  const targetDir = benchmarkRoot
    ? join(searchDir, benchmarkRoot)
    : searchDir;

  try {
    const output = execSync(
      `rg -n --no-heading --max-count ${k * 2} "${query.replace(/"/g, '\\"')}" .`,
      { cwd: targetDir, encoding: "utf-8", stdio: "pipe", timeout: 10000 }
    );

    const lines = output.trim().split("\n").filter(Boolean);
    results = lines.slice(0, k).map((line) => {
      const colonIdx = line.indexOf(":");
      const file = line.substring(0, colonIdx);
      const rest = line.substring(colonIdx + 1);
      const colonIdx2 = rest.indexOf(":");
      const lineNum = parseInt(rest.substring(0, colonIdx2), 10);
      const match = rest.substring(colonIdx2 + 1);
      return { file, line: lineNum, match: match.trim() };
    });
  } catch {
    results = [];
  }

  const latency_ms = performance.now() - start;
  return { results, latency_ms };
}

// ---------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------

/** Normalize path separators to forward slash and strip leading ./ */
function normalizePath(p: string): string {
  return p.replace(/\\/g, "/").replace(/^\.\//, "");
}

function computeRecallAtK(
  retrieved: RgResult[],
  relevant: string[],
  k: number
): number {
  if (relevant.length === 0) return 0;
  const retrievedPaths = new Set(
    retrieved.slice(0, k).map((r) => normalizePath(r.file))
  );
  let hits = 0;
  for (const r of relevant) {
    const nr = normalizePath(r);
    for (const rp of retrievedPaths) {
      if (rp.endsWith(nr) || nr.endsWith(rp)) {
        hits++;
        break;
      }
    }
  }
  return hits / relevant.length;
}

function computeMRR(retrieved: RgResult[], relevant: string[]): number {
  for (let i = 0; i < retrieved.length; i++) {
    const rf = normalizePath(retrieved[i].file);
    for (const r of relevant) {
      const nr = normalizePath(r);
      if (rf.endsWith(nr) || nr.endsWith(rf)) {
        return 1 / (i + 1);
      }
    }
  }
  return 0;
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

function main() {
  const args = process.argv.slice(2);
  let pilot = false;
  let cacheDir = ".bench-cache";
  let inputFile = "";
  let k = 10;
  let outputFile = "baseline-rg-report.json";

  for (let i = 2; i < args.length; i++) {
    switch (args[i]) {
      case "--pilot":
        pilot = true;
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
    }
  }

  if (!inputFile) {
    inputFile = pilot ? "benchmarks/semble/fixtures.json" : "benchmarks/semble/fixtures.json";
  }

  const fixture: Fixture = JSON.parse(readFileSync(resolve(inputFile), "utf-8"));
  console.log(`Running baseline with k=${k} on ${fixture.annotations.length} queries`);

  const results: QueryResult[] = [];

  for (const ann of fixture.annotations) {
    const repo = fixture.repos.find((r) => r.name === ann.repo_name);
    if (!repo) continue;

    const searchDir = join(resolve(cacheDir), repo.name);
    if (!existsSync(searchDir)) {
      console.warn(`  Skipping ${ann.repo_name} (not cloned)`);
      continue;
    }

    const { results: rgResults, latency_ms } = rgSearch(
      ann.query,
      searchDir,
      repo.benchmark_root,
      k
    );

    const allRelevant = [
      ...ann.relevant.map((r) => r.path),
      ...ann.secondary.map((r) => r.path),
    ];

    const recall_at_k = computeRecallAtK(rgResults, allRelevant, k);
    const mrr = computeMRR(rgResults, allRelevant);

    results.push({
      query: ann.query,
      repo_name: ann.repo_name,
      category: ann.category,
      latency_ms,
      results: rgResults,
      recall_at_k,
      mrr,
    });
  }

  // Aggregate
  const latencies = results.map((r) => r.latency_ms).sort((a, b) => a - b);
  const p50 = latencies[Math.floor(latencies.length * 0.5)] ?? 0;
  const p95 = latencies[Math.floor(latencies.length * 0.95)] ?? 0;

  const byCategory: Record<
    string,
    { recall: number; mrr: number; latency: number; count: number }
  > = {};
  for (const r of results) {
    if (!byCategory[r.category]) {
      byCategory[r.category] = { recall: 0, mrr: 0, latency: 0, count: 0 };
    }
    byCategory[r.category].recall += r.recall_at_k;
    byCategory[r.category].mrr += r.mrr;
    byCategory[r.category].latency += r.latency_ms;
    byCategory[r.category].count++;
  }

  const aggregateByCategory: Record<
    string,
    { recall: number; mrr: number; latency: number }
  > = {};
  for (const [cat, data] of Object.entries(byCategory)) {
    aggregateByCategory[cat] = {
      recall: data.recall / data.count,
      mrr: data.mrr / data.count,
      latency: data.latency / data.count,
    };
  }

  const report: BaselineReport = {
    timestamp: new Date().toISOString(),
    fixture_source: fixture.source.name,
    k,
    total_queries: results.length,
    results,
    aggregate: {
      mean_recall_at_k:
        results.reduce((s, r) => s + r.recall_at_k, 0) / results.length,
      mean_mrr: results.reduce((s, r) => s + r.mrr, 0) / results.length,
      mean_latency_ms:
        results.reduce((s, r) => s + r.latency_ms, 0) / results.length,
      p50_latency_ms: p50,
      p95_latency_ms: p95,
      by_category: aggregateByCategory,
    },
  };

  writeFileSync(resolve(outputFile), JSON.stringify(report, null, 2) + "\n");

  console.log(`\n=== Baseline Results ===`);
  console.log(`Queries: ${results.length}`);
  console.log(`Mean recall@${k}: ${report.aggregate.mean_recall_at_k.toFixed(3)}`);
  console.log(`Mean MRR: ${report.aggregate.mean_mrr.toFixed(3)}`);
  console.log(
    `Latency: p50=${p50.toFixed(1)}ms, p95=${p95.toFixed(1)}ms`
  );
  console.log(`\nBy category:`);
  for (const [cat, data] of Object.entries(aggregateByCategory)) {
    console.log(
      `  ${cat}: recall=${data.recall.toFixed(3)} mrr=${data.mrr.toFixed(3)} latency=${data.latency.toFixed(1)}ms`
    );
  }
}

main();
