#!/usr/bin/env bun
/**
 * Token efficiency benchmarking for AFT search.
 *
 * Measures recall@token_budget curves: how many tokens are needed
 * to achieve target recall across different modes.
 *
 * Usage:
 *   bun run benchmarks/semble/token-efficiency.ts [options]
 *
 * Options:
 *   --cache-dir <dir>    Repo cache directory (default: .bench-cache)
 *   --input <file>       Fixture file (default: fixtures.json)
 *   --output <file>      Output report (default: token-efficiency-report.json)
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
    secondary: Array<{ path: string }>;
    category: string;
    repo_name: string;
  }>;
}

interface TokenBudgetResult {
  budget: number;
  recall: number;
  latency_ms: number;
}

interface QueryTokenResult {
  query: string;
  repo_name: string;
  category: string;
  budget_curve: TokenBudgetResult[];
}

interface TokenEfficiencyReport {
  timestamp: string;
  fixture_source: string;
  budgets: number[];
  results: QueryTokenResult[];
  aggregate_curve: TokenBudgetResult[];
  by_category: Record<string, TokenBudgetResult[]>;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function normalizePath(p: string): string {
  return p.replace(/\\/g, "/").replace(/^\.\//, "");
}

function estimateTokens(text: string): number {
  // Rough estimate: ~4 chars per token for code
  return Math.ceil(text.length / 4);
}

// ---------------------------------------------------------------------------
// Lexical search with token budget
// ---------------------------------------------------------------------------

function rgSearchWithBudget(
  query: string,
  searchDir: string,
  benchmarkRoot: string | null,
  maxTokens: number
): { files: string[]; latency_ms: number } {
  const targetDir = benchmarkRoot ? join(searchDir, benchmarkRoot) : searchDir;
  const start = performance.now();
  let files: string[] = [];
  let totalTokens = 0;

  try {
    const output = execSync(
      `rg -n --no-heading --max-count 100 "${query.replace(/"/g, '\\"')}" .`,
      { cwd: targetDir, encoding: "utf-8", stdio: "pipe", timeout: 10000 }
    );

    const lines = output.trim().split("\n").filter(Boolean);
    for (const line of lines) {
      const tokens = estimateTokens(line);
      if (totalTokens + tokens > maxTokens) break;
      totalTokens += tokens;
      const ci = line.indexOf(":");
      const file = line.substring(0, ci);
      if (!files.includes(file)) files.push(file);
    }
  } catch {}

  return { files, latency_ms: performance.now() - start };
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

function main() {
  const args = process.argv.slice(2);
  let cacheDir = ".bench-cache";
  let inputFile = "benchmarks/semble/fixtures.json";
  let outputFile = "token-efficiency-report.json";

  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--cache-dir":
        cacheDir = args[++i];
        break;
      case "--input":
        inputFile = args[++i];
        break;
      case "--output":
        outputFile = args[++i];
        break;
    }
  }

  const fixture: Fixture = JSON.parse(readFileSync(resolve(inputFile), "utf-8"));

  // Token budgets to test: 100, 500, 1K, 2K, 5K, 10K, 50K tokens
  const budgets = [100, 500, 1000, 2000, 5000, 10000, 50000];

  console.log(
    `Running token efficiency benchmark: ${fixture.annotations.length} queries, budgets: ${budgets.join(", ")}}`
  );

  const results: QueryTokenResult[] = [];

  for (const ann of fixture.annotations) {
    const repo = fixture.repos.find((r) => r.name === ann.repo_name);
    if (!repo) continue;

    const repoDir = join(resolve(cacheDir), repo.name);
    if (!existsSync(repoDir)) continue;

    const allRelevant = [
      ...ann.relevant.map((r) => r.path),
      ...ann.secondary.map((r) => r.path),
    ];

    const budgetCurve: TokenBudgetResult[] = [];

    for (const budget of budgets) {
      const { files, latency_ms } = rgSearchWithBudget(
        ann.query,
        repoDir,
        repo.benchmark_root,
        budget
      );

      // Calculate recall
      let hits = 0;
      for (const r of allRelevant) {
        const nr = normalizePath(r);
        for (const f of files) {
          const nf = normalizePath(f);
          if (nf.endsWith(nr) || nr.endsWith(nf)) {
            hits++;
            break;
          }
        }
      }
      const recall = allRelevant.length > 0 ? hits / allRelevant.length : 0;

      budgetCurve.push({ budget, recall, latency_ms });
    }

    results.push({
      query: ann.query,
      repo_name: ann.repo_name,
      category: ann.category,
      budget_curve: budgetCurve,
    });
  }

  // Aggregate curve (mean recall per budget)
  const aggregateCurve: TokenBudgetResult[] = budgets.map((budget) => {
    const matching = results
      .map((r) => r.budget_curve.find((b) => b.budget === budget))
      .filter(Boolean) as TokenBudgetResult[];
    return {
      budget,
      recall:
        matching.reduce((s, b) => s + b.recall, 0) / matching.length,
      latency_ms:
        matching.reduce((s, b) => s + b.latency_ms, 0) / matching.length,
    };
  });

  // By category
  const categories = [...new Set(results.map((r) => r.category))];
  const byCategory: Record<string, TokenBudgetResult[]> = {};
  for (const cat of categories) {
    const catResults = results.filter((r) => r.category === cat);
    byCategory[cat] = budgets.map((budget) => {
      const matching = catResults
        .map((r) => r.budget_curve.find((b) => b.budget === budget))
        .filter(Boolean) as TokenBudgetResult[];
      return {
        budget,
        recall:
          matching.length > 0
            ? matching.reduce((s, b) => s + b.recall, 0) / matching.length
            : 0,
        latency_ms:
          matching.length > 0
            ? matching.reduce((s, b) => s + b.latency_ms, 0) / matching.length
            : 0,
      };
    });
  }

  const report: TokenEfficiencyReport = {
    timestamp: new Date().toISOString(),
    fixture_source: fixture.source.name,
    budgets,
    results,
    aggregate_curve: aggregateCurve,
    by_category: byCategory,
  };

  writeFileSync(resolve(outputFile), JSON.stringify(report, null, 2) + "\n");

  console.log(`\n=== Token Efficiency Results ===`);
  console.log(`Budget | Recall  | Latency`);
  for (const point of aggregateCurve) {
    console.log(
      `${String(point.budget).padStart(6)} | ${(point.recall * 100).toFixed(1).padStart(5)}% | ${point.latency_ms.toFixed(1).padStart(6)}ms`
    );
  }
}

main();
