/**
 * Schema-versioned benchmark report writer.
 *
 * Produces JSON, JSONL, and Markdown outputs suitable for human review
 * and CI/agent parsing.
 */

import { writeFileSync } from "fs";
import type { QueryAttempt, AggregateResult } from "./bench-metrics";

// ---------------------------------------------------------------------------
// Report schema
// ---------------------------------------------------------------------------

export interface BenchmarkReport {
  schema_version: number;
  generated_at: string;
  command: string;
  config: {
    profile: string;
    suites: string[];
    modes: string[];
    k: number;
    candidate_pool: number;
    rerank_pool: number;
    binary_path: string;
    backends: string[];
  };
  mode_mapping: Record<string, { aft_tool: string; rust_command: string; description: string }>;
  environment: {
    aft_version?: string;
    node_version: string;
    platform: string;
    date: string;
  };
  repos: Array<{
    name: string;
    url?: string;
    revision?: string | null;
    pin_status?: string;
  }>;
  preflight: Array<{
    mode: string;
    suite: string;
    status: string;
    reason?: string;
  }>;
  suites: Record<string, {
    aggregates: AggregateResult[];
    attempts: QueryAttempt[];
    seed_count: number;
    reviewed_count: number;
  }>;
  failures: Array<{
    mode: string;
    suite: string;
    query_id: string;
    status: string;
    reason?: string;
  }>;
  warnings: string[];
  summary: {
    total_queries: number;
    total_attempts: number;
    total_ok: number;
    total_empty: number;
    total_error: number;
    total_unavailable: number;
    suites_run: string[];
    modes_run: string[];
  };
}

// ---------------------------------------------------------------------------
// Mode-to-tool mapping (for report metadata)
// ---------------------------------------------------------------------------

export const MODE_MAPPING: Record<string, { aft_tool: string; rust_command: string; description: string }> = {
  rg: { aft_tool: "bash (ripgrep)", rust_command: "(external)", description: "Baseline lexical search via ripgrep" },
  "aft-grep": { aft_tool: "grep", rust_command: "grep", description: "Trigram-indexed lexical search" },
  fts5_search: { aft_tool: "aft_fts5_search", rust_command: "fts5_search", description: "FTS5 full-text search" },
  fts5_find_symbol_exact: { aft_tool: "aft_find_symbol (exact)", rust_command: "fts5_find_symbol", description: "Exact symbol lookup via FTS5" },
  fts5_find_symbol_prefix: { aft_tool: "aft_find_symbol (prefix)", rust_command: "fts5_find_symbol", description: "Prefix symbol lookup via FTS5" },
  glob: { aft_tool: "glob", rust_command: "glob", description: "File path pattern matching" },
  ast_search: { aft_tool: "ast_grep_search", rust_command: "ast_search", description: "Structural AST pattern search" },
  semantic_m2v: { aft_tool: "aft_search", rust_command: "semantic_search", description: "Dense embedding search (Model2Vec)" },
  semantic_fe: { aft_tool: "aft_search", rust_command: "semantic_search", description: "Dense embedding search (FastEmbed)" },
  semantic_api: { aft_tool: "aft_search", rust_command: "semantic_search", description: "Dense embedding search (OpenAI-compatible API)" },
  hybrid: { aft_tool: "aft_search + aft_fts5_search", rust_command: "semantic_search + fts5_search", description: "RRF fusion of lexical + semantic" },
  rerank: { aft_tool: "aft_search + /v1/rerank", rust_command: "semantic_search + rerank endpoint", description: "Post-retrieval reranking" },
};

// ---------------------------------------------------------------------------
// Report writer
// ---------------------------------------------------------------------------

export function writeJsonReport(
  attempts: QueryAttempt[],
  aggregates: AggregateResult[],
  config: BenchmarkReport["config"],
  preflight: BenchmarkReport["preflight"],
  repos: BenchmarkReport["repos"],
  warnings: string[],
  outputPath: string,
): void {
  // Group attempts by suite
  const suiteMap = new Map<string, QueryAttempt[]>();
  for (const a of attempts) {
    if (!suiteMap.has(a.suite)) suiteMap.set(a.suite, []);
    suiteMap.get(a.suite)!.push(a);
  }

  // Group aggregates by suite
  const aggBySuite = new Map<string, AggregateResult[]>();
  for (const agg of aggregates) {
    if (!aggBySuite.has(agg.suite)) aggBySuite.set(agg.suite, []);
    aggBySuite.get(agg.suite)!.push(agg);
  }

  // Build suite sections
  const suites: BenchmarkReport["suites"] = {};
  for (const [suite, suiteAttempts] of suiteMap) {
    suites[suite] = {
      aggregates: aggBySuite.get(suite) || [],
      attempts: suiteAttempts.sort((a, b) => a.query_id.localeCompare(b.query_id) || a.mode.localeCompare(b.mode)),
      seed_count: suiteAttempts.filter((a) => (a as any).review_status === "seed").length,
      reviewed_count: suiteAttempts.filter((a) => (a as any).review_status === "reviewed").length,
    };
  }

  // Collect failures
  const failures: BenchmarkReport["failures"] = [];
  for (const a of attempts) {
    if (a.status === "error" || a.status === "timeout") {
      failures.push({
        mode: a.mode,
        suite: a.suite,
        query_id: a.query_id,
        status: a.status,
        reason: a.reason,
      });
    }
  }

  // Summary
  const summary: BenchmarkReport["summary"] = {
    total_queries: new Set(attempts.map((a) => a.query_id)).size,
    total_attempts: attempts.length,
    total_ok: attempts.filter((a) => a.status === "ok").length,
    total_empty: attempts.filter((a) => a.status === "empty").length,
    total_error: attempts.filter((a) => a.status === "error").length,
    total_unavailable: attempts.filter((a) => a.status === "unavailable").length,
    suites_run: [...new Set(attempts.map((a) => a.suite))].sort(),
    modes_run: [...new Set(attempts.map((a) => a.mode))].sort(),
  };

  const report: BenchmarkReport = {
    schema_version: 1,
    generated_at: new Date().toISOString(),
    command: process.argv.join(" "),
    config,
    mode_mapping: MODE_MAPPING,
    environment: {
      node_version: process.version,
      platform: process.platform,
      date: new Date().toISOString().split("T")[0],
    },
    repos,
    preflight,
    suites,
    failures,
    warnings,
    summary,
  };

  writeFileSync(outputPath, JSON.stringify(report, null, 2));
  console.log(`Report written to ${outputPath}`);
}

// ---------------------------------------------------------------------------
// JSONL writer (one attempt per line)
// ---------------------------------------------------------------------------

export function writeJsonlReport(attempts: QueryAttempt[], outputPath: string): void {
  const lines = attempts
    .sort((a, b) => a.suite.localeCompare(b.suite) || a.mode.localeCompare(b.mode) || a.query_id.localeCompare(b.query_id))
    .map((a) => JSON.stringify(a));
  writeFileSync(outputPath, lines.join("\n") + "\n");
  console.log(`JSONL written to ${outputPath} (${lines.length} attempts)`);
}

// ---------------------------------------------------------------------------
// Markdown writer
// ---------------------------------------------------------------------------

export function writeMarkdownReport(
  attempts: QueryAttempt[],
  aggregates: AggregateResult[],
  outputPath: string,
): void {
  const lines: string[] = [];
  lines.push("# AFT Benchmark Report");
  lines.push("");
  lines.push(`Generated: ${new Date().toISOString()}`);
  lines.push("");

  // Mode-tool mapping reference
  lines.push("## Mode → AFT Tool → Rust Command");
  lines.push("");
  lines.push("| Benchmark Mode | AFT Tool (OpenCode/Pi) | Rust Command | Description |");
  lines.push("|----------------|----------------------|--------------|-------------|");
  for (const [mode, mapping] of Object.entries(MODE_MAPPING)) {
    lines.push(`| ${mode} | ${mapping.aft_tool} | ${mapping.rust_command} | ${mapping.description} |`);
  }
  lines.push("");

  // Group by suite
  const suiteAggs = new Map<string, AggregateResult[]>();
  for (const agg of aggregates) {
    if (!suiteAggs.has(agg.suite)) suiteAggs.set(agg.suite, []);
    suiteAggs.get(agg.suite)!.push(agg);
  }

  for (const [suite, aggs] of suiteAggs) {
    lines.push(`## ${suite.replace(/_/g, " ").toUpperCase()}`);
    lines.push("");
    lines.push("| Mode | Attempted | OK | Empty | Error | Unavail | Recall | MRR | nDCG | P50 ms | P95 ms |");
    lines.push("|------|-----------|-----|-------|-------|---------|--------|-----|------|--------|--------|");

    for (const agg of aggs) {
      lines.push(
        `| ${agg.mode} | ${agg.total_attempted} | ${agg.ok} | ${agg.empty} | ${agg.error} | ${agg.unavailable} | ${agg.recall.toFixed(3)} | ${agg.mrr.toFixed(3)} | ${agg.ndcg.toFixed(3)} | ${agg.p50_ms.toFixed(0)} | ${agg.p95_ms.toFixed(0)} |`,
      );
    }
    lines.push("");
  }

  // Failure summary
  const failures = attempts.filter((a) => a.status === "error" || a.status === "timeout");
  if (failures.length > 0) {
    lines.push("## Failures");
    lines.push("");
    for (const f of failures) {
      lines.push(`- **${f.suite}/${f.mode}** ${f.query_id}: ${f.status}${f.reason ? ` (${f.reason})` : ""}`);
    }
    lines.push("");
  }

  writeFileSync(outputPath, lines.join("\n"));
  console.log(`Markdown written to ${outputPath}`);
}

// ---------------------------------------------------------------------------
// Baseline comparison
// ---------------------------------------------------------------------------

export interface ThresholdConfig {
  recall_drop?: number;      // max allowed recall drop (absolute)
  mrr_drop?: number;         // max allowed MRR drop (absolute)
  ndcg_drop?: number;        // max allowed nDCG drop (absolute)
  empty_rate_increase?: number; // max allowed empty rate increase (absolute)
}

export interface ComparisonResult {
  suite: string;
  mode: string;
  baseline: AggregateResult;
  current: AggregateResult;
  regressions: string[];
  improvements: string[];
}

export function compareBaseline(
  baseline: BenchmarkReport,
  current: BenchmarkReport,
  thresholds: ThresholdConfig = {},
): ComparisonResult[] {
  const results: ComparisonResult[] = [];

  // Index baseline aggregates by suite+mode
  const baselineMap = new Map<string, AggregateResult>();
  for (const agg of Object.values(baseline.suites)) {
    for (const a of agg.aggregates) {
      baselineMap.set(`${a.suite}|${a.mode}`, a);
    }
  }

  // Compare current against baseline
  const seen = new Set<string>();
  for (const agg of Object.values(current.suites)) {
    for (const a of agg.aggregates) {
      const key = `${a.suite}|${a.mode}`;
      seen.add(key);
      const base = baselineMap.get(key);
      if (!base) continue;

      const regressions: string[] = [];
      const improvements: string[] = [];

      if (thresholds.recall_drop !== undefined) {
        const delta = a.recall - base.recall;
        if (delta < -thresholds.recall_drop) regressions.push(`recall dropped ${(-delta).toFixed(3)} (threshold: ${thresholds.recall_drop})`);
        else if (delta > 0.01) improvements.push(`recall improved ${delta.toFixed(3)}`);
      }

      if (thresholds.mrr_drop !== undefined) {
        const delta = a.mrr - base.mrr;
        if (delta < -thresholds.mrr_drop) regressions.push(`MRR dropped ${(-delta).toFixed(3)}`);
        else if (delta > 0.01) improvements.push(`MRR improved ${delta.toFixed(3)}`);
      }

      if (thresholds.ndcg_drop !== undefined) {
        const delta = a.ndcg - base.ndcg;
        if (delta < -thresholds.ndcg_drop) regressions.push(`nDCG dropped ${(-delta).toFixed(3)}`);
        else if (delta > 0.01) improvements.push(`nDCG improved ${delta.toFixed(3)}`);
      }

      results.push({ suite: a.suite, mode: a.mode, baseline: base, current: a, regressions, improvements });
    }
  }

  return results;
}

export function printComparison(results: ComparisonResult[]): void {
  const hasRegressions = results.some((r) => r.regressions.length > 0);
  const hasImprovements = results.some((r) => r.improvements.length > 0);

  console.log("\n=== Baseline Comparison ===");
  for (const r of results) {
    if (r.regressions.length > 0) {
      console.log(`  ✗ ${r.suite}/${r.mode}:`);
      for (const reg of r.regressions) console.log(`    - ${reg}`);
    } else if (r.improvements.length > 0) {
      console.log(`  ✓ ${r.suite}/${r.mode}:`);
      for (const imp of r.improvements) console.log(`    + ${imp}`);
    }
  }
  if (!hasRegressions && !hasImprovements) {
    console.log("  No significant changes from baseline.");
  }
  console.log("");
}
