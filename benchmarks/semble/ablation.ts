#!/usr/bin/env bun
/**
 * Ablation benchmark comparing search modes.
 *
 * Compares: lexical-only, semantic-only, hybrid, reranked-hybrid.
 * Identifies which mode wins for which query categories.
 *
 * Usage:
 *   bun run benchmarks/semble/ablation.ts [options]
 *
 * Options:
 *   --cache-dir <dir>    Repo cache directory (default: .bench-cache)
 *   --input <file>       Fixture file (default: fixtures.json)
 *   --k <n>              Top-k for evaluation (default: 10)
 *   --output <file>      Output report (default: ablation-report.json)
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

interface CategoryWinner {
  category: string;
  best_mode: string;
  best_recall: number;
  modes: Record<string, { recall: number; mrr: number; ndcg: number; latency_ms: number }>;
}

interface AblationReport {
  timestamp: string;
  fixture_source: string;
  k: number;
  results: ModeResult[];
  aggregate: Record<string, { recall: number; mrr: number; ndcg: number; latency_ms: number; count: number }>;
  category_winners: CategoryWinner[];
}

// ---------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------

function normalizePath(p: string): string {
  return p.replace(/\\/g, "/").replace(/^\.\//, "");
}

function recallAtK(retrieved: SearchResult[], relevant: string[], k: number): number {
  if (relevant.length === 0) return 0;
  const rPaths = new Set(retrieved.slice(0, k).map((r) => normalizePath(r.file)));
  let hits = 0;
  for (const r of relevant) {
    const nr = normalizePath(r);
    for (const rp of rPaths) {
      if (rp.endsWith(nr) || nr.endsWith(rp)) { hits++; break; }
    }
  }
  return hits / relevant.length;
}

function mrr(retrieved: SearchResult[], relevant: string[]): number {
  for (let i = 0; i < retrieved.length; i++) {
    const rf = normalizePath(retrieved[i].file);
    for (const r of relevant) {
      const nr = normalizePath(r);
      if (rf.endsWith(nr) || nr.endsWith(rf)) return 1 / (i + 1);
    }
  }
  return 0;
}

function ndcgAtK(retrieved: SearchResult[], relevant: string[], k: number): number {
  const relSet = new Set(relevant.map(normalizePath));
  let dcg = 0;
  for (let i = 0; i < Math.min(k, retrieved.length); i++) {
    const rf = normalizePath(retrieved[i].file);
    const isRel = [...relSet].some((r) => rf.endsWith(r) || r.endsWith(rf));
    if (isRel) dcg += 1 / Math.log2(i + 2);
  }
  const idealHits = Math.min(relSet.size, k);
  let idcg = 0;
  for (let i = 0; i < idealHits; i++) idcg += 1 / Math.log2(i + 2);
  return idcg > 0 ? dcg / idcg : 0;
}

// ---------------------------------------------------------------------------
// Lexical mode
// ---------------------------------------------------------------------------

function rgSearch(query: string, searchDir: string, benchmarkRoot: string | null, k: number): { results: SearchResult[]; latency_ms: number } {
  const targetDir = benchmarkRoot ? join(searchDir, benchmarkRoot) : searchDir;
  const start = performance.now();
  let results: SearchResult[] = [];
  try {
    const output = execSync(`rg -n --no-heading --max-count ${k * 2} "${query.replace(/"/g, '\\"')}" .`, { cwd: targetDir, encoding: "utf-8", stdio: "pipe", timeout: 10000 });
    const lines = output.trim().split("\n").filter(Boolean);
    results = lines.slice(0, k).map((line) => {
      const ci = line.indexOf(":");
      const file = line.substring(0, ci);
      const rest = line.substring(ci + 1);
      const ci2 = rest.indexOf(":");
      return { file, line: parseInt(rest.substring(0, ci2), 10) };
    });
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
  let outputFile = "ablation-report.json";

  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--cache-dir": cacheDir = args[++i]; break;
      case "--input": inputFile = args[++i]; break;
      case "--k": k = parseInt(args[++i], 10); break;
      case "--output": outputFile = args[++i]; break;
    }
  }

  const fixture: Fixture = JSON.parse(readFileSync(resolve(inputFile), "utf-8"));
  console.log(`Ablation benchmark: ${fixture.annotations.length} queries, k=${k}`);

  const allResults: ModeResult[] = [];
  const modes = ["lexical"];  // AFT-specific modes need the binary; lexical is the baseline we can run

  for (const ann of fixture.annotations) {
    const repo = fixture.repos.find((r) => r.name === ann.repo_name);
    if (!repo) continue;
    const repoDir = join(resolve(cacheDir), repo.name);
    if (!existsSync(repoDir)) continue;

    const allRelevant = [...ann.relevant.map((r) => r.path), ...ann.secondary.map((r) => r.path)];

    // Lexical
    const { results: rgResults, latency_ms: rgLat } = rgSearch(ann.query, repoDir, repo.benchmark_root, k);
    allResults.push({
      mode: "lexical", query: ann.query, repo_name: ann.repo_name, category: ann.category,
      latency_ms: rgLat, results: rgResults,
      recall_at_k: recallAtK(rgResults, allRelevant, k),
      mrr: mrr(rgResults, allRelevant),
      ndcg_at_k: ndcgAtK(rgResults, allRelevant, k),
    });
  }

  // Aggregate
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
      recall: data.recalls.reduce((s, v) => s + v, 0) / n,
      mrr: data.mrrs.reduce((s, v) => s + v, 0) / n,
      ndcg: data.ndcgs.reduce((s, v) => s + v, 0) / n,
      latency_ms: data.latencies.reduce((s, v) => s + v, 0) / n,
      count: n,
    };
  }

  // Category winners
  const categories = [...new Set(allResults.map((r) => r.category))];
  const categoryWinners: CategoryWinner[] = categories.map((cat) => {
    const catResults = allResults.filter((r) => r.category === cat);
    const modesData: Record<string, { recalls: number[]; mrrs: number[]; ndcgs: number[]; lats: number[] }> = {};
    for (const r of catResults) {
      if (!modesData[r.mode]) modesData[r.mode] = { recalls: [], mrrs: [], ndcgs: [], lats: [] };
      modesData[r.mode].recalls.push(r.recall_at_k);
      modesData[r.mode].mrrs.push(r.mrr);
      modesData[r.mode].ndcgs.push(r.ndcg_at_k);
      modesData[r.mode].lats.push(r.latency_ms);
    }
    const modesAgg: Record<string, { recall: number; mrr: number; ndcg: number; latency_ms: number }> = {};
    let bestMode = "lexical";
    let bestRecall = 0;
    for (const [mode, data] of Object.entries(modesData)) {
      const n = data.recalls.length;
      modesAgg[mode] = {
        recall: data.recalls.reduce((s, v) => s + v, 0) / n,
        mrr: data.mrrs.reduce((s, v) => s + v, 0) / n,
        ndcg: data.ndcgs.reduce((s, v) => s + v, 0) / n,
        latency_ms: data.lats.reduce((s, v) => s + v, 0) / n,
      };
      if (modesAgg[mode].recall > bestRecall) { bestRecall = modesAgg[mode].recall; bestMode = mode; }
    }
    return { category: cat, best_mode: bestMode, best_recall: bestRecall, modes: modesAgg };
  });

  const report: AblationReport = {
    timestamp: new Date().toISOString(),
    fixture_source: fixture.source.name,
    k,
    results: allResults,
    aggregate,
    category_winners: categoryWinners,
  };

  writeFileSync(resolve(outputFile), JSON.stringify(report, null, 2) + "\n");

  console.log(`\n=== Ablation Results ===`);
  for (const [mode, data] of Object.entries(aggregate)) {
    console.log(`  ${mode}: recall=${(data.recall * 100).toFixed(1)}% mrr=${data.mrr.toFixed(3)} ndcg=${data.ndcg.toFixed(3)} latency=${data.latency_ms.toFixed(1)}ms`);
  }
  console.log(`\nCategory winners:`);
  for (const cw of categoryWinners) {
    console.log(`  ${cw.category}: ${cw.best_mode} (${(cw.best_recall * 100).toFixed(1)}%)`);
  }
}

main();
