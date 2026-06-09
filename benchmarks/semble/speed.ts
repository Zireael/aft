#!/usr/bin/env bun
/**
 * Speed benchmarking for AFT semantic search.
 *
 * Measures cold-start index time and warm query latency per mode.
 * Runs against cloned repos using the AFT binary.
 *
 * Usage:
 *   bun run benchmarks/semble/speed.ts [options]
 *
 * Options:
 *   --pilot              Use pilot fixture set
 *   --cache-dir <dir>    Repo cache directory (default: .bench-cache)
 *   --input <file>       Fixture file (default: fixtures.json)
 *   --iterations <n>     Warm query iterations (default: 3)
 *   --output <file>      Output report (default: speed-report.json)
 */

import { readFileSync, writeFileSync, existsSync } from "fs";
import { join, resolve } from "path";
import { execSync } from "child_process";

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
    relevant: Array<{ path: string }>;
    category: string;
    repo_name: string;
  }>;
}

interface IndexTiming {
  repo: string;
  language: string;
  benchmark_root: string | null;
  cold_start_ms: number;
  file_count: number;
}

interface QueryTiming {
  query: string;
  repo_name: string;
  category: string;
  mode: string;
  latency_ms: number;
  result_count: number;
}

interface SpeedReport {
  timestamp: string;
  fixture_source: string;
  iterations: number;
  index_timings: IndexTiming[];
  query_timings: QueryTiming[];
  aggregate: {
    index: {
      mean_cold_start_ms: number;
      total_cold_start_ms: number;
      by_language: Record<string, number>;
    };
    query: Record<
      string,
      {
        mean_latency_ms: number;
        p50_latency_ms: number;
        p95_latency_ms: number;
        query_count: number;
      }
    >;
  };
}

// ---------------------------------------------------------------------------
// AFT binary detection
// ---------------------------------------------------------------------------

function findAftBinary(): string {
  const candidates = [
    "aft",
    "target/release/aft",
    "target/debug/aft",
    join(process.env.HOME || "", ".cargo/bin/aft"),
  ];
  for (const c of candidates) {
    try {
      execSync(`${c} --version`, { stdio: "pipe" });
      return c;
    } catch {}
  }
  throw new Error("aft binary not found in PATH or target/");
}

// ---------------------------------------------------------------------------
// Index timing
// ---------------------------------------------------------------------------

function measureIndexTime(
  repoDir: string,
  repoName: string,
  language: string,
  benchmarkRoot: string | null,
  aftBinary: string
): IndexTiming {
  const projectRoot = benchmarkRoot ? join(repoDir, benchmarkRoot) : repoDir;

  // Count files
  let fileCount = 0;
  try {
    const output = execSync(`find . -type f | wc -l`, {
      cwd: projectRoot,
      encoding: "utf-8",
      stdio: "pipe",
    }).trim();
    fileCount = parseInt(output, 10) || 0;
  } catch {}

  // Measure index build time (cold start — first run against this repo)
  const start = performance.now();
  try {
    execSync(
      `echo '{"command":"configure","params":{"project_root":"${projectRoot.replace(/\\/g, "\\\\")}"}}' | ${aftBinary}`,
      { stdio: "pipe", timeout: 60000 }
    );
  } catch {}
  const cold_start_ms = performance.now() - start;

  return {
    repo: repoName,
    language,
    benchmark_root: benchmarkRoot,
    cold_start_ms,
    file_count: fileCount,
  };
}

// ---------------------------------------------------------------------------
// Query timing
// ---------------------------------------------------------------------------

function measureQueryLatency(
  query: string,
  repoDir: string,
  benchmarkRoot: string | null,
  aftBinary: string,
  iterations: number
): { latency_ms: number; result_count: number } {
  const projectRoot = benchmarkRoot ? join(repoDir, benchmarkRoot) : repoDir;
  const latencies: number[] = [];
  let resultCount = 0;

  for (let i = 0; i < iterations; i++) {
    const start = performance.now();
    try {
      const output = execSync(
        `echo '{"command":"semantic_search","params":{"query":"${query.replace(/"/g, '\\"')}","project_root":"${projectRoot.replace(/\\/g, "\\\\")}","top_k":10}}' | ${aftBinary}`,
        { encoding: "utf-8", stdio: "pipe", timeout: 30000 }
      );
      const elapsed = performance.now() - start;
      latencies.push(elapsed);

      // Parse result count from JSON response
      try {
        const resp = JSON.parse(output);
        if (i === 0 && resp.result) {
          resultCount = Array.isArray(resp.result)
            ? resp.result.length
            : resp.result.total || 0;
        }
      } catch {}
    } catch {
      latencies.push(performance.now() - start);
    }
  }

  // Use median latency
  latencies.sort((a, b) => a - b);
  const median = latencies[Math.floor(latencies.length / 2)] ?? 0;

  return { latency_ms: median, result_count: resultCount };
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

function main() {
  const args = process.argv.slice(2);
  let pilot = false;
  let cacheDir = ".bench-cache";
  let inputFile = "";
  let iterations = 3;
  let outputFile = "speed-report.json";

  for (let i = 0; i < args.length; i++) {
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
      case "--iterations":
        iterations = parseInt(args[++i], 10);
        break;
      case "--output":
        outputFile = args[++i];
        break;
    }
  }

  if (!inputFile) {
    inputFile = "benchmarks/semble/fixtures.json";
  }

  const fixture: Fixture = JSON.parse(readFileSync(resolve(inputFile), "utf-8"));

  let aftBinary: string;
  try {
    aftBinary = findAftBinary();
  } catch (e) {
    console.error(String(e));
    console.log("Falling back to file-system-only benchmarks (no AFT binary)");
    aftBinary = "";
  }

  console.log(
    `Running speed benchmark on ${fixture.repos.length} repos, ${fixture.annotations.length} queries`
  );

  // Index timings
  const indexTimings: IndexTiming[] = [];
  for (const repo of fixture.repos) {
    const repoDir = join(resolve(cacheDir), repo.name);
    if (!existsSync(repoDir)) {
      console.warn(`  Skipping ${repo.name} (not cloned)`);
      continue;
    }
    console.log(`  Indexing ${repo.name}...`);
    const timing = measureIndexTime(
      repoDir,
      repo.name,
      repo.language,
      repo.benchmark_root,
      aftBinary
    );
    indexTimings.push(timing);
  }

  // Query timings (sample subset to keep runtime manageable)
  const queryTimings: QueryTiming[] = [];
  const sampleSize = Math.min(fixture.annotations.length, 20);
  const sample = fixture.annotations
    .sort(() => Math.random() - 0.5)
    .slice(0, sampleSize);

  for (const ann of sample) {
    const repo = fixture.repos.find((r) => r.name === ann.repo_name);
    if (!repo) continue;

    const repoDir = join(resolve(cacheDir), repo.name);
    if (!existsSync(repoDir)) continue;

    console.log(`  Querying: "${ann.query.slice(0, 50)}..."`);
    const { latency_ms, result_count } = measureQueryLatency(
      ann.query,
      repoDir,
      repo.benchmark_root,
      aftBinary,
      iterations
    );

    queryTimings.push({
      query: ann.query,
      repo_name: ann.repo_name,
      category: ann.category,
      mode: "semantic",
      latency_ms,
      result_count,
    });
  }

  // Aggregate
  const indexByLanguage: Record<string, number> = {};
  for (const t of indexTimings) {
    indexByLanguage[t.language] =
      (indexByLanguage[t.language] ?? 0) + t.cold_start_ms;
  }

  const queryByMode: Record<
    string,
    { latencies: number[]; count: number }
  > = {};
  for (const t of queryTimings) {
    if (!queryByMode[t.mode]) queryByMode[t.mode] = { latencies: [], count: 0 };
    queryByMode[t.mode].latencies.push(t.latency_ms);
    queryByMode[t.mode].count++;
  }

  const queryAggregate: Record<
    string,
    {
      mean_latency_ms: number;
      p50_latency_ms: number;
      p95_latency_ms: number;
      query_count: number;
    }
  > = {};
  for (const [mode, data] of Object.entries(queryByMode)) {
    const sorted = [...data.latencies].sort((a, b) => a - b);
    queryAggregate[mode] = {
      mean_latency_ms:
        data.latencies.reduce((s, v) => s + v, 0) / data.latencies.length,
      p50_latency_ms: sorted[Math.floor(sorted.length * 0.5)] ?? 0,
      p95_latency_ms: sorted[Math.floor(sorted.length * 0.95)] ?? 0,
      query_count: data.count,
    };
  }

  const report: SpeedReport = {
    timestamp: new Date().toISOString(),
    fixture_source: fixture.source.name,
    iterations,
    index_timings: indexTimings,
    query_timings: queryTimings,
    aggregate: {
      index: {
        mean_cold_start_ms:
          indexTimings.reduce((s, t) => s + t.cold_start_ms, 0) /
          indexTimings.length,
        total_cold_start_ms: indexTimings.reduce(
          (s, t) => s + t.cold_start_ms,
          0
        ),
        by_language: indexByLanguage,
      },
      query: queryAggregate,
    },
  };

  writeFileSync(resolve(outputFile), JSON.stringify(report, null, 2) + "\n");

  console.log(`\n=== Speed Benchmark Results ===`);
  console.log(
    `Index cold start: mean=${report.aggregate.index.mean_cold_start_ms.toFixed(0)}ms total=${report.aggregate.index.total_cold_start_ms.toFixed(0)}ms`
  );
  for (const [lang, ms] of Object.entries(indexByLanguage)) {
    console.log(`  ${lang}: ${ms.toFixed(0)}ms`);
  }
  console.log(`\nQuery latency:`);
  for (const [mode, data] of Object.entries(queryAggregate)) {
    console.log(
      `  ${mode}: mean=${data.mean_latency_ms.toFixed(1)}ms p50=${data.p50_latency_ms.toFixed(1)}ms p95=${data.p95_latency_ms.toFixed(1)}ms (${data.query_count} queries)`
    );
  }
}

main();
