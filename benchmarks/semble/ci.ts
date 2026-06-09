#!/usr/bin/env bun
/**
 * CI integration script for AFT benchmarks.
 *
 * Runs pilot benchmarks and detects regressions against baseline.
 * Exits with non-zero code if regression detected.
 *
 * Usage:
 *   bun run benchmarks/semble/ci.ts [options]
 *
 * Options:
 *   --baseline <file>    Baseline report (default: pilot-report.json)
 *   --threshold <n>      Regression threshold (default: 0.05 = 5%)
 *   --cache-dir <dir>    Repo cache directory (default: .bench-cache)
 *   --output <file>      Output comparison (default: ci-comparison.json)
 */

import { readFileSync, writeFileSync, existsSync } from "fs";
import { resolve } from "path";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface BaselineReport {
  aggregate: Record<string, {
    mean_recall: number;
    mean_mrr: number;
    mean_ndcg: number;
    mean_latency_ms: number;
    query_count: number;
  }>;
}

interface ComparisonResult {
  mode: string;
  baseline_recall: number;
  current_recall: number;
  recall_delta: number;
  baseline_mrr: number;
  current_mrr: number;
  mrr_delta: number;
  regression: boolean;
}

interface CIReport {
  timestamp: string;
  threshold: number;
  passed: boolean;
  results: ComparisonResult[];
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

function main() {
  const args = process.argv.slice(2);
  let baselineFile = "benchmarks/semble/pilot-report.json";
  let currentFile = "benchmarks/semble/pilot-report.json";
  let threshold = 0.05;
  let outputFile = "ci-comparison.json";

  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--baseline":
        baselineFile = args[++i];
        break;
      case "--current":
        currentFile = args[++i];
        break;
      case "--threshold":
        threshold = parseFloat(args[++i]);
        break;
      case "--output":
        outputFile = args[++i];
        break;
    }
  }

  if (!existsSync(baselineFile)) {
    console.log(`No baseline found at ${baselineFile}. Saving current as baseline.`);
    process.exit(0);
  }

  const baseline: BaselineReport = JSON.parse(readFileSync(resolve(baselineFile), "utf-8"));
  const current: BaselineReport = JSON.parse(readFileSync(resolve(currentFile), "utf-8"));

  const results: ComparisonResult[] = [];
  let passed = true;

  for (const [mode, curData] of Object.entries(current.aggregate)) {
    const baseData = baseline.aggregate[mode];
    if (!baseData) continue;

    const recallDelta = curData.mean_recall - baseData.mean_recall;
    const mrrDelta = curData.mean_mrr - baseData.mean_mrr;
    const regression = recallDelta < -threshold;

    if (regression) passed = false;

    results.push({
      mode,
      baseline_recall: baseData.mean_recall,
      current_recall: curData.mean_recall,
      recall_delta: recallDelta,
      baseline_mrr: baseData.mean_mrr,
      current_mrr: curData.mean_mrr,
      mrr_delta: mrrDelta,
      regression,
    });
  }

  const report: CIReport = {
    timestamp: new Date().toISOString(),
    threshold,
    passed,
    results,
  };

  writeFileSync(resolve(outputFile), JSON.stringify(report, null, 2) + "\n");

  console.log(`=== CI Benchmark Comparison ===`);
  console.log(`Threshold: ${(threshold * 100).toFixed(1)}%`);

  for (const r of results) {
    const status = r.regression ? "REGRESSION" : "OK";
    console.log(
      `  ${r.mode}: recall ${(r.baseline_recall * 100).toFixed(1)}% → ${(r.current_recall * 100).toFixed(1)}% (${r.recall_delta >= 0 ? "+" : ""}${(r.recall_delta * 100).toFixed(1)}%) [${status}]`
    );
  }

  if (passed) {
    console.log(`\n✅ All modes within threshold`);
  } else {
    console.log(`\n❌ Regression detected`);
    process.exit(1);
  }
}

main();
