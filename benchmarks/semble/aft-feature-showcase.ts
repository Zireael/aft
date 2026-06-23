#!/usr/bin/env bun
/**
 * AFT feature showcase.
 *
 * This is intentionally separate from pilot.ts. pilot.ts is a scored benchmark
 * engine; this file is a user-facing feature tour that explains what the new
 * retrieval intelligence features do and shows their observed behavior against
 * baseline AFT search tools.
 */

import { existsSync, mkdirSync, statSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import { basename, join, resolve } from "path";
import { AftSession, type AftResponse } from "./aft-ndjson";

export type CommandStatus = "ok" | "error" | "skipped";

export interface ShowcaseConfig {
  binary: string;
  projectRoot: string;
  storageDir: string;
  query: string;
  diagnosticQuery: string;
  symbol: string;
  expectedFile: string;
  topK: number;
  timeoutMs: number;
  diagnosticTimeoutMs: number;
  color: boolean;
  jsonOutput?: string;
  markdownOutput?: string;
  skipFts5Index: boolean;
}

export interface ComparisonRow {
  label: string;
  command: string;
  status: CommandStatus;
  latencyMs: number;
  resultCount: number;
  expectedRank?: number;
  topFile?: string;
  speedupVsBaseline?: number;
  qualityDeltaVsBaseline?: number;
  searchPlanIntent?: string;
  activeSafetyLane?: string;
  laneCount?: number;
  rankingFeatures?: string[];
  enrichmentStates?: string[];
  snippetCount?: number;
  tokenCount?: number;
  contextBudget?: {
    totalTokens?: number;
    perCandidateTokens?: number;
    softOverflowTokens?: number;
  };
  qualityNotes: string[];
  error?: string;
}

export interface FeatureCard {
  title: string;
  status: "available" | "degraded" | "missing";
  whyItMatters: string;
  evidence: string[];
}

export interface DiagnosticRow {
  label: string;
  status: CommandStatus;
  latencyMs: number;
  summary: string;
  whyItMatters: string;
  error?: string;
}

export interface ShowcaseReport {
  generatedAt: string;
  binary: string;
  projectRoot: string;
  query: string;
  expectedFile: string;
  topK: number;
  comparisons: ComparisonRow[];
  featureCards: FeatureCard[];
  diagnostics: DiagnosticRow[];
  recommendations: string[];
}

interface RenderOptions {
  color: boolean;
}

interface TimedResponse {
  response: AftResponse;
  latencyMs: number;
}

  const DEFAULT_QUERY = "CandidateEntry";
  const DEFAULT_DIAGNOSTIC_QUERY = "E0433 unresolved import";
  const DEFAULT_SYMBOL = "handle_semantic_search";
  const DEFAULT_TOP_K = 10;
  const DEFAULT_TIMEOUT_MS = 60_000;
const DEFAULT_DIAGNOSTIC_TIMEOUT_MS = 180_000;

function usage(): string {
  return [
    "Usage: bun run benchmarks/semble/aft-feature-showcase.ts --binary <path> [options]",
    "",
    "Options:",
    "  --binary <path>          AFT binary path (default: aft)",
    "  --project-root <dir>     Project to showcase (default: current directory)",
    "  --storage-dir <dir>      AFT storage directory (default: OS temp)",
    `  --query <text>           Main comparison query (default: ${DEFAULT_QUERY})`,
    `  --diagnostic-query <q>   Diagnostic ranking query (default: ${DEFAULT_DIAGNOSTIC_QUERY})`,
    `  --symbol <name>          Symbol for impact analysis (default: ${DEFAULT_SYMBOL})`,
    "  --expected-file <path>   Expected relevant file for rank comparison",
    `  --top-k <n>              Number of results to compare (default: ${DEFAULT_TOP_K})`,
    "  --json-output <file>     Write structured report JSON",
    "  --markdown-output <file> Write rendered report text",
    "  --skip-fts5-index        Skip fts5_index update",
    "  --timeout-ms <n>         Per-command timeout (default: 60000)",
    "  --diagnostic-timeout-ms <n>  Timeout for FTS5/doctor/context diagnostics (default: 180000)",
    "  --no-color              Disable ANSI colors",
    "  --help                  Show this help",
  ].join("\n");
}

export function parseArgs(argv: string[]): ShowcaseConfig {
  const config: ShowcaseConfig = {
    binary: "aft",
    projectRoot: process.cwd(),
    storageDir: "",
    query: DEFAULT_QUERY,
    diagnosticQuery: DEFAULT_DIAGNOSTIC_QUERY,
    symbol: DEFAULT_SYMBOL,
    expectedFile: "",
    topK: DEFAULT_TOP_K,
    timeoutMs: DEFAULT_TIMEOUT_MS,
    diagnosticTimeoutMs: DEFAULT_DIAGNOSTIC_TIMEOUT_MS,
    color: true,
    skipFts5Index: false,
  };

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    switch (arg) {
      case "--binary":
        config.binary = argv[++i];
        break;
      case "--project-root":
        config.projectRoot = argv[++i];
        break;
      case "--storage-dir":
        config.storageDir = argv[++i];
        break;
      case "--query":
        config.query = argv[++i];
        break;
      case "--diagnostic-query":
        config.diagnosticQuery = argv[++i];
        break;
      case "--symbol":
        config.symbol = argv[++i];
        break;
      case "--expected-file":
        config.expectedFile = argv[++i];
        break;
      case "--top-k":
        config.topK = parsePositiveInt(argv[++i], "--top-k", 1, 100);
        break;
      case "--timeout-ms":
        config.timeoutMs = parsePositiveInt(argv[++i], "--timeout-ms", 1_000, 600_000);
        break;
      case "--diagnostic-timeout-ms":
        config.diagnosticTimeoutMs = parsePositiveInt(argv[++i], "--diagnostic-timeout-ms", 1_000, 600_000);
        break;
      case "--json-output":
        config.jsonOutput = argv[++i];
        break;
      case "--markdown-output":
        config.markdownOutput = argv[++i];
        break;
      case "--skip-fts5-index":
        config.skipFts5Index = true;
        break;
      case "--no-color":
        config.color = false;
        break;
      case "--help":
      case "-h":
        console.log(usage());
        process.exit(0);
      default:
        throw new Error(`Unknown argument: ${arg}\n\n${usage()}`);
    }
  }

  config.projectRoot = resolve(config.projectRoot);
  if (!config.storageDir) {
    const projectName = basename(config.projectRoot).replace(/[^a-zA-Z0-9_.-]/g, "_") || "project";
    config.storageDir = join(tmpdir(), `aft-feature-showcase-${projectName}`);
  }
  config.storageDir = resolve(config.storageDir);
  return config;
}

function parsePositiveInt(value: string | undefined, name: string, min: number, max: number): number {
  const parsed = Number.parseInt(value || "", 10);
  if (!Number.isFinite(parsed) || parsed < min || parsed > max) {
    throw new Error(`${name} must be an integer between ${min} and ${max}; got ${value}`);
  }
  return parsed;
}

export async function runShowcase(config: ShowcaseConfig): Promise<ShowcaseReport> {
  validateConfig(config);
  mkdirSync(config.storageDir, { recursive: true });

  const comparisons: ComparisonRow[] = [];
  const diagnostics: DiagnosticRow[] = [];
  let riSearch: AftResponse | null = null;

  comparisons.push(await runComparisonCommand(config, "AFT-GREP baseline", "grep", {
    command: "grep",
    pattern: config.query,
    max_results: config.topK,
  }));

  comparisons.push(await runComparisonCommand(config, "AFT-FTS5 baseline", "fts5_search", {
    command: "fts5_search",
    query: config.query,
    scope: "all",
    top_k: config.topK,
  }, { indexFts5: !config.skipFts5Index }));

  const session = new AftSession(config.binary);
  try {
    await configureSession(session, config, { storageSuffix: "ri", fts5: true, intelligence: true });
    if (!config.skipFts5Index) {
      diagnostics.push(await diagnosticCall(session, "FTS5 index update", {
        command: "fts5_index",
        action: "update",
      }, Math.max(config.diagnosticTimeoutMs, 60_000), summarizeFts5Index, "Builds the SQLite FTS5 symbol/body/path index used by exact lookup, prefix lookup, full-text search, and hybrid retrieval."));
    }

    diagnostics.push(await diagnosticCall(session, "FTS5 doctor", {
      command: "fts5_doctor",
    }, config.diagnosticTimeoutMs, summarizeFts5Doctor, "Confirms whether FTS5 is compiled, enabled, populated, and healthy before judging search quality."));

    diagnostics.push(await diagnosticCall(session, "FTS5 symbol lookup", {
      command: "fts5_find_symbol",
      name: config.symbol,
      mode: "exact",
      top_k: config.topK,
    }, config.diagnosticTimeoutMs, summarizeFts5FindSymbol, "Shows exact symbol lookup over the FTS5 symbol table, the clearest win over plain grep for code navigation."));

    diagnostics.push(await diagnosticCall(session, "FTS5 read symbol", {
      command: "fts5_read_symbol",
      name: config.symbol,
      context_lines: 2,
    }, config.diagnosticTimeoutMs, summarizeFts5ReadSymbol, "Reads canonical source for a symbol from the index, turning lookup results into usable code context."));

    diagnostics.push(await diagnosticCall(session, "Semantic doctor", {
      command: "semantic_doctor",
      probe_provider: false,
    }, config.diagnosticTimeoutMs, summarizeSemanticDoctor, "Reports semantic backend, index, and metrics health so quality issues can be separated from provider/config problems."));

    const semantic = await timedCall(session, {
      command: "semantic_search",
      query: config.query,
      top_k: config.topK,
      diagnostics: true,
    }, config.timeoutMs);
    riSearch = semantic.response;
    comparisons.push(toComparison("RI v2 semantic_search", "semantic_search", semantic, config));

    const budgetSemantic = await timedCall(session, {
      command: "semantic_search",
      query: config.query,
      top_k: config.topK,
      diagnostics: true,
      context_budget_enabled: true,
      profile: "agent_fast",
      context_total_tokens: 4096,
      context_per_candidate_tokens: 384,
      context_soft_overflow_tokens: 128,
    }, config.timeoutMs);
    comparisons.push(toComparison("RI v2 token-budget semantic_search", "semantic_search", budgetSemantic, config));

    addSpeedAndQualityDeltas(comparisons);

    diagnostics.push(await diagnosticCall(session, "Explain search", {
      command: "explain_search",
      query: config.query,
    }, config.diagnosticTimeoutMs, summarizeExplain, "Explains lane weights and safety lanes, so users know why the search behaved the way it did."));

    if (config.expectedFile) {
      diagnostics.push(await diagnosticCall(session, "Why missed", {
        command: "why_missed",
        query: config.query,
        expected_file: config.expectedFile,
      }, config.diagnosticTimeoutMs, summarizeWhyMissed, "Shows whether an expected file entered the candidate pool and which lanes missed it."));
    }

    diagnostics.push(await diagnosticCall(session, "Orient", {
      command: "aft_orient",
      query: config.query,
      depth: 2,
    }, config.diagnosticTimeoutMs, summarizeOrient, "Turns search hits into an entry-point map instead of a flat list."));

    diagnostics.push(await diagnosticCall(session, "Impact delta", {
      command: "aft_impact_delta",
      symbol: config.symbol,
      change_type: "signature",
    }, config.diagnosticTimeoutMs, summarizeImpact, "Estimates blast radius and mutation risk for a symbol-level change."));

    diagnostics.push(await diagnosticCall(session, "Context pack", {
      command: "aft_context_pack",
      query: config.query,
      token_budget: 4000,
    }, config.diagnosticTimeoutMs, summarizeContextPack, "Packages relevant code into a bounded context budget for agent workflows."));
  } finally {
    session.close();
  }

  return {
    generatedAt: new Date().toISOString(),
    binary: config.binary,
    projectRoot: config.projectRoot,
    query: config.query,
    expectedFile: config.expectedFile,
    topK: config.topK,
    comparisons,
    featureCards: buildFeatureCards(riSearch, comparisons, diagnostics, config),
    diagnostics,
    recommendations: buildRecommendations(comparisons, diagnostics, config.expectedFile),
  };
}

async function runComparisonCommand(
  config: ShowcaseConfig,
  label: string,
  commandName: string,
  command: Record<string, unknown>,
  options: { indexFts5?: boolean } = {},
): Promise<ComparisonRow> {
  const session = new AftSession(config.binary);
  const start = performance.now();
  try {
    await configureSession(session, config, {
      storageSuffix: commandName.replace(/[^a-zA-Z0-9_.-]/g, "_"),
      fts5: commandName === "fts5_search",
      intelligence: false,
    });
    if (options.indexFts5) {
      await timedCall(session, { command: "fts5_index", action: "update" }, Math.max(config.timeoutMs, 60_000));
    }
    const timed = await timedCall(session, command, config.timeoutMs);
    return toComparison(label, commandName, timed, config);
  } catch (error) {
    return {
      label,
      command: commandName,
      status: "error",
      latencyMs: performance.now() - start,
      resultCount: 0,
      qualityNotes: ["baseline command failed or timed out"],
      error: String(error),
    };
  } finally {
    session.close();
  }
}

async function configureSession(
  session: AftSession,
  config: ShowcaseConfig,
  options: { storageSuffix: string; fts5: boolean; intelligence: boolean },
): Promise<void> {
  await session.call({
    command: "configure",
    harness: "opencode",
    project_root: config.projectRoot,
    storage_dir: join(config.storageDir, options.storageSuffix),
    search_index: true,
    semantic_search: false,
    fts5: { enabled: options.fts5 },
    intelligence: options.intelligence ? { retrieval_intelligence_v2: true } : undefined,
  }, config.timeoutMs);
}

function validateConfig(config: ShowcaseConfig): void {
  if (config.binary !== "aft") {
    try {
      statSync(config.binary);
    } catch {
      throw new Error(`AFT binary not found: ${config.binary}`);
    }
  }
  if (!existsSync(config.projectRoot)) {
    throw new Error(`Project root not found: ${config.projectRoot}`);
  }
}

async function timedCall(session: AftSession, command: Record<string, unknown>, timeoutMs: number): Promise<TimedResponse> {
  const start = performance.now();
  const response = await session.call(command, timeoutMs);
  return { response, latencyMs: performance.now() - start };
}

async function diagnosticCall(
  session: AftSession,
  label: string,
  command: Record<string, unknown>,
  timeoutMs: number,
  summarize: (response: AftResponse) => string,
  whyItMatters: string,
): Promise<DiagnosticRow> {
  try {
    const timed = await timedCall(session, command, timeoutMs);
    return {
      label,
      status: timed.response.success === false ? "error" : "ok",
      latencyMs: timed.latencyMs,
      summary: summarize(timed.response),
      whyItMatters,
      error: timed.response.success === false ? String(timed.response.message || timed.response.code || "command failed") : undefined,
    };
  } catch (error) {
    return {
      label,
      status: "error",
      latencyMs: 0,
      summary: "Command failed before returning a structured response.",
      whyItMatters,
      error: String(error),
    };
  }
}

function toComparison(label: string, command: string, timed: TimedResponse, config: ShowcaseConfig): ComparisonRow {
  const response = timed.response;
  const results = extractResults(response);
  const expectedRank = config.expectedFile ? findExpectedRank(results, config.expectedFile) : undefined;
  const plan = response.search_plan_debug as any;
  const provenance = response.retrieval_intelligence_provenance as any;
  const rankingFeatures = uniqueStrings((provenance?.ranking_features || []).flatMap((entry: any) => {
    return (entry.applied || []).map((feature: any) => String(feature.feature || "")).filter(Boolean);
  }));
  const enrichmentStates = uniqueStrings(results.map((result: any) => String(result.enrichment_state || "")).filter(Boolean));
  const laneCount = countLanes(response);
  const snippetCount = results.filter((result: any) => typeof result.snippet === "string" && result.snippet.length > 0).length;
  const tokenCount = results.reduce((sum: number, result: any) => sum + approxTokens(String(result.snippet || "")), 0);
  const contextBudget = extractContextBudget(response);

  return {
    label,
    command,
    status: response.success === false ? "error" : "ok",
    latencyMs: timed.latencyMs,
    resultCount: results.length,
    expectedRank,
    topFile: firstFile(results),
    searchPlanIntent: typeof plan?.intent === "string" ? plan.intent : undefined,
    activeSafetyLane: typeof plan?.active_safety_lane === "string" ? plan.active_safety_lane : undefined,
    laneCount,
    rankingFeatures,
    enrichmentStates,
    snippetCount,
    tokenCount,
    contextBudget,
    qualityNotes: buildQualityNotes(label, response, expectedRank, config.expectedFile),
    error: response.success === false ? String(response.message || response.code || "command failed") : undefined,
  };
}

function extractContextBudget(response: AftResponse): ComparisonRow["contextBudget"] {
  const budget = (response.search_plan_debug as any)?.context_budget;
  if (!budget || typeof budget !== "object") return undefined;
  return {
    totalTokens: typeof budget.total_tokens === "number" ? budget.total_tokens : undefined,
    perCandidateTokens: typeof budget.per_candidate_tokens === "number" ? budget.per_candidate_tokens : undefined,
    softOverflowTokens: typeof budget.soft_overflow_tokens === "number" ? budget.soft_overflow_tokens : undefined,
  };
}

function extractResults(response: AftResponse): any[] {
  for (const key of ["results", "evidence", "matches"]) {
    const value = (response as any)[key];
    if (Array.isArray(value)) return value;
  }
  return [];
}

function firstFile(results: any[]): string | undefined {
  const first = results[0];
  return first ? String(first.file || first.file_path || first.path || "") || undefined : undefined;
}

function findExpectedRank(results: any[], expectedFile: string): number | undefined {
  const expected = normalizePath(expectedFile);
  if (!expected) return undefined;
  const idx = results.findIndex((result) => {
    const file = normalizePath(String(result.file || result.file_path || result.path || ""));
    return file === expected || file.endsWith(`/${expected}`) || expected.endsWith(`/${file}`);
  });
  return idx >= 0 ? idx + 1 : undefined;
}

function normalizePath(path: string): string {
  return path.replace(/\\/g, "/").replace(/^\/\?\//, "").replace(/^\.\//, "").toLowerCase();
}

function countLanes(response: AftResponse): number | undefined {
  const plan = response.search_plan_debug as any;
  if (plan?.lane_weights && typeof plan.lane_weights === "object") {
    return Object.keys(plan.lane_weights).length;
  }
  const provenance = response.retrieval_intelligence_provenance as any;
  const lanes = new Set<string>();
  for (const contribution of provenance?.lane_contributions || []) {
    for (const lane of contribution.lanes || []) {
      if (lane.lane) lanes.add(String(lane.lane));
    }
  }
  return lanes.size > 0 ? lanes.size : undefined;
}

function buildQualityNotes(label: string, response: AftResponse, expectedRank: number | undefined, expectedFile: string): string[] {
  const notes: string[] = [];
  if (expectedFile && expectedRank) notes.push(`expected file found at rank ${expectedRank}`);
  if (expectedFile && !expectedRank) notes.push("expected file not in top results");
  if (response.search_plan_debug) notes.push("search plan emitted");
  if (label.includes("token-budget")) notes.push("token-budget context request enabled");
  if (response.retrieval_intelligence_provenance) notes.push("RI provenance emitted");
  if ((response as any).semantic_unavailable) notes.push("semantic unavailable surfaced explicitly");
  if ((response as any).lexical_only_fallback) notes.push("lexical fallback surfaced explicitly");
  if (label.toLowerCase().includes("baseline")) notes.push("baseline comparison path");
  return notes;
}

function addSpeedAndQualityDeltas(rows: ComparisonRow[]): void {
  const baseline = rows.find((row) => row.status === "ok" && row.latencyMs > 0);
  if (!baseline) return;
  for (const row of rows) {
    if (row === baseline || row.status !== "ok" || row.latencyMs <= 0) continue;
    row.speedupVsBaseline = baseline.latencyMs / row.latencyMs;
    if (baseline.expectedRank && row.expectedRank) {
      row.qualityDeltaVsBaseline = baseline.expectedRank - row.expectedRank;
    } else if (!baseline.expectedRank && row.expectedRank) {
      row.qualityDeltaVsBaseline = rows.length + 1 - row.expectedRank;
    }
  }
}

function buildFeatureCards(response: AftResponse | null, comparisons: ComparisonRow[], diagnostics: DiagnosticRow[], config: ShowcaseConfig): FeatureCard[] {
  const cards: FeatureCard[] = [];
  const plan = response?.search_plan_debug as any;
  const provenance = response?.retrieval_intelligence_provenance as any;
  const rankingFeatures = uniqueStrings((provenance?.ranking_features || []).flatMap((entry: any) => {
    return (entry.applied || []).map((feature: any) => String(feature.feature || "")).filter(Boolean);
  }));

  cards.push({
    title: "SearchPlan",
    status: plan ? "available" : "missing",
    whyItMatters: "Shows query intent, lane weights, safety lanes, and retrieval strategy instead of hiding search as a black box.",
    evidence: plan
      ? [`intent ${plan.intent || "unknown"}`, `${Object.keys(plan.lane_weights || {}).length} lane weights`, `safety lane ${plan.active_safety_lane || "unknown"}`]
      : ["No search_plan_debug field in semantic_search output."],
  });

  cards.push({
    title: "Candidate provenance",
    status: provenance ? "available" : "missing",
    whyItMatters: "Explains which lanes contributed each result and whether graph/context/ranking features affected the final order.",
    evidence: provenance
      ? [`${(provenance.lane_contributions || []).length} lane contribution rows`, `${rankingFeatures.length} ranking feature types`]
      : ["No retrieval_intelligence_provenance field in semantic_search output."],
  });

  cards.push({
    title: "Definition-aware ranking",
    status: rankingFeatures.length > 0 ? "available" : "degraded",
    whyItMatters: "Moves likely definitions and symbol matches above generic mentions, which matters most for agent code navigation.",
    evidence: rankingFeatures.length > 0 ? rankingFeatures : ["No ranking features reported for this query."],
  });

  cards.push({
    title: "Diagnostics and context tools",
    status: diagnostics.some((row) => row.status === "ok") ? "available" : "missing",
    whyItMatters: "Turns search from a list of hits into explainable workflow primitives: orient, why-missed, impact, and context pack.",
    evidence: diagnostics.map((row) => `${row.label}: ${row.status}`),
  });

  const budgetRow = comparisons.find((row) => row.label.includes("token-budget"));
  cards.push({
    title: "Token-budget context",
    status: budgetRow?.contextBudget ? "available" : "degraded",
    whyItMatters: "Shows the branch's context-volume improvement separately from ranking quality: more selected snippets can feed reranking and agent context without changing base recall.",
    evidence: budgetRow
      ? [
        `${budgetRow.snippetCount || 0} snippets selected`,
        `${budgetRow.tokenCount || 0} snippet tokens`,
        budgetRow.contextBudget
          ? `budget total=${budgetRow.contextBudget.totalTokens ?? "?"}, per_candidate=${budgetRow.contextBudget.perCandidateTokens ?? "?"}, soft_overflow=${budgetRow.contextBudget.softOverflowTokens ?? 0}`
          : "context budget debug not present",
      ]
      : ["Token-budget semantic search comparison did not run."],
  });

  cards.push({
    title: "Telemetry privacy posture",
    status: existsSync(join(config.storageDir, "ri", "aft.db")) ? "available" : "degraded",
    whyItMatters: "Runtime search writes operational telemetry while defaulting to hashed queries, giving performance insight without raw-query storage by default.",
    evidence: existsSync(join(config.storageDir, "ri", "aft.db"))
      ? [`telemetry database created at ${join(config.storageDir, "ri", "aft.db")}`]
      : ["No telemetry database observed in the showcase storage directory."],
  });

  return cards;
}

function summarizeExplain(response: AftResponse): string {
  const result = (response as any).explain_search_result;
  if (!result) return String(response.message || "No explain_search_result payload.");
  const lanes = Array.isArray(result.lane_weights) ? result.lane_weights.length : Object.keys(result.lane_weights || {}).length;
  return `intent ${result.query_intent || "unknown"}, ${lanes} lane weights, safety lane ${result.active_safety_lane || "unknown"}`;
}

function summarizeWhyMissed(response: AftResponse): string {
  const result = (response as any).why_missed_result;
  if (!result) return String(response.message || "No why_missed_result payload.");
  const pool = result.was_in_candidate_pool ? "entered candidate pool" : "not in candidate pool";
  const missing = Array.isArray(result.missing_from_lanes) ? result.missing_from_lanes.length : 0;
  return `${pool}; ${missing} lanes reported miss details`;
}

function summarizeOrient(response: AftResponse): string {
  const result = (response as any).orient_result;
  if (!result) return String(response.message || "No orient_result payload.");
  return `${(result.primary_files || []).length} primary files, ${(result.entry_symbols || []).length} entry symbols`;
}

function summarizeImpact(response: AftResponse): string {
  const result = (response as any).impact_delta_result;
  if (!result) return String(response.message || "No impact_delta_result payload.");
  const count = result.blast_radius?.symbol_count ?? "unknown";
  return `graph ${result.graph?.health || "unknown"}, blast radius ${count}, mutation risk ${result.mutation_risk || "unknown"}`;
}

function summarizeContextPack(response: AftResponse): string {
  const result = (response as any).context_pack_result;
  if (!result) return String(response.message || "No context_pack_result payload.");
  return `${(result.pack || []).length} items, ${result.tokens_used || 0}/${result.token_budget || "?"} tokens used`;
}

function summarizeFts5Index(response: AftResponse): string {
  const text = typeof response.text === "string" ? response.text.trim() : "";
  if (text) return firstLine(text);
  const indexed = (response as any).indexed_files ?? (response as any).files_indexed;
  const symbols = (response as any).symbols_indexed ?? (response as any).symbol_count;
  return `indexed files=${indexed ?? "?"}, symbols=${symbols ?? "?"}`;
}

function summarizeFts5Doctor(response: AftResponse): string {
  const text = typeof response.text === "string" ? response.text.trim() : "";
  if (text) return firstLine(text);
  const compiled = (response as any).compiled ?? (response as any).fts5_compiled;
  const enabled = (response as any).enabled ?? (response as any).fts5_enabled;
  const status = (response as any).status || (response.success === false ? "error" : "ok");
  return `status=${status}, compiled=${compiled ?? "?"}, enabled=${enabled ?? "?"}`;
}

function summarizeFts5FindSymbol(response: AftResponse): string {
  const results = extractResults(response);
  if (results.length === 0) return String(response.message || "No symbols returned.");
  const first = results[0] as any;
  const name = first.symbol_name || first.name || first.symbol || "?";
  const file = first.file || first.file_path || first.path || "?";
  return `${results.length} symbol candidates; top ${name} in ${file}`;
}

function summarizeFts5ReadSymbol(response: AftResponse): string {
  const text = typeof response.text === "string" ? response.text.trim() : "";
  if (text) return firstLine(text);
  const file = (response as any).file || (response as any).file_path || (response as any).path;
  const source = (response as any).source || (response as any).content || "";
  return file ? `${file}, ${source.split("\n").filter(Boolean).length} source lines` : String(response.message || "No symbol source returned.");
}

function summarizeSemanticDoctor(response: AftResponse): string {
  if (typeof response.summary_line === "string" && response.summary_line.length > 0) {
    return response.summary_line;
  }
  const status = typeof response.status === "string" ? response.status : response.success === false ? "error" : "ok";
  const config = (response as any).config || {};
  const backend = config.backend || "?";
  const model = config.model || "?";
  return `status=${status}, backend=${backend}, model=${model}`;
}

function firstLine(text: string): string {
  return text.split(/\r?\n/).find((line) => line.trim().length > 0)?.trim() || text;
}

function approxTokens(text: string): number {
  if (!text) return 0;
  return text.split(/\s+/).filter(Boolean).length;
}

function buildRecommendations(comparisons: ComparisonRow[], diagnostics: DiagnosticRow[], expectedFile: string): string[] {
  const recommendations: string[] = [];
  const ri = comparisons.find((row) => row.command === "semantic_search");
  const fastest = comparisons.filter((row) => row.status === "ok").sort((a, b) => a.latencyMs - b.latencyMs)[0];
  if (ri?.expectedRank === 1) {
    recommendations.push("Use RI v2 search for agent navigation when top-rank correctness matters.");
  } else if (ri?.expectedRank) {
    recommendations.push(`RI v2 found the expected file at rank ${ri.expectedRank}; inspect ranking features before tuning.`);
  } else if (expectedFile) {
    recommendations.push("RI v2 did not place the expected file in the top K for this query; try a more specific symbol/query or run against a cleaner project root before using this as quality evidence.");
  } else {
    recommendations.push("Provide --expected-file for a stronger quality comparison on your target query.");
  }
  if (fastest) {
    recommendations.push(`${fastest.label} was fastest in this run; use speed together with rank quality, not by itself.`);
  }
  if (diagnostics.some((row) => row.status === "error")) {
    recommendations.push("At least one diagnostic command failed; use the error text to separate feature availability from search quality.");
  } else {
    recommendations.push("Diagnostics, orientation, impact, and context-pack commands are suitable for workflow demos on this project.");
  }
  return recommendations;
}

export function renderReport(report: ShowcaseReport, options: RenderOptions = { color: true }): string {
  const c = colors(options.color);
  const lines: string[] = [];
  lines.push(`${c.bold}AFT Feature Showcase${c.reset}`);
  lines.push(`${c.dim}Generated ${report.generatedAt}${c.reset}`);
  lines.push("");
  lines.push(`${c.bold}Target${c.reset}`);
  lines.push(`  Binary: ${report.binary}`);
  lines.push(`  Project: ${report.projectRoot}`);
  lines.push(`  Query: ${report.query}`);
  if (report.expectedFile) lines.push(`  Expected file: ${report.expectedFile}`);
  lines.push(`  Top K: ${report.topK}`);
  lines.push("");

  lines.push(`${c.bold}Baseline vs Retrieval Intelligence${c.reset}`);
  lines.push(formatComparisonTable(report.comparisons));
  lines.push("");
  for (const row of report.comparisons) {
    lines.push(`${c.bold}${row.label}${c.reset}`);
    lines.push(`  Command: ${row.command}`);
    lines.push(`  Status: ${statusText(row.status, c)}`);
    lines.push(`  Latency: ${formatMs(row.latencyMs)}`);
    lines.push(`  Results: ${row.resultCount}`);
    if (row.topFile) lines.push(`  Top file: ${row.topFile}`);
    if (row.expectedRank) lines.push(`  Expected file rank: #${row.expectedRank}`);
    if (row.speedupVsBaseline) lines.push(`  Speed: ${row.speedupVsBaseline.toFixed(2)}x faster than baseline`);
    if (typeof row.qualityDeltaVsBaseline === "number") lines.push(`  Rank lift: ${formatRankLift(row.qualityDeltaVsBaseline)}`);
    if (row.searchPlanIntent) lines.push(`  SearchPlan: ${row.searchPlanIntent}, safety lane ${row.activeSafetyLane || "unknown"}, ${row.laneCount || 0} lanes`);
    if (row.rankingFeatures?.length) lines.push(`  Ranking features: ${row.rankingFeatures.join(", ")}`);
    if (row.enrichmentStates?.length) lines.push(`  Enrichment states: ${row.enrichmentStates.join(", ")}`);
    if (typeof row.snippetCount === "number") lines.push(`  Context: ${row.snippetCount} snippets, ${row.tokenCount || 0} snippet tokens`);
    if (row.contextBudget) {
      lines.push(`  Budget: total=${row.contextBudget.totalTokens ?? "?"}, per_candidate=${row.contextBudget.perCandidateTokens ?? "?"}, soft_overflow=${row.contextBudget.softOverflowTokens ?? 0}`);
    }
    if (row.qualityNotes.length) lines.push(`  Notes: ${row.qualityNotes.join("; ")}`);
    if (row.error) lines.push(`  Error: ${row.error}`);
    lines.push("");
  }

  lines.push(`${c.bold}Feature Cards${c.reset}`);
  for (const card of report.featureCards) {
    lines.push(`  ${cardStatus(card.status, c)} ${card.title}`);
    lines.push(`     Why it matters: ${card.whyItMatters}`);
    lines.push(`     Evidence: ${card.evidence.join("; ")}`);
  }
  lines.push("");

  lines.push(`${c.bold}Workflow Diagnostics${c.reset}`);
  for (const row of report.diagnostics) {
    lines.push(`  ${statusText(row.status, c)} ${row.label} (${formatMs(row.latencyMs)})`);
    lines.push(`     ${row.summary}`);
    lines.push(`     Why it matters: ${row.whyItMatters}`);
    if (row.error) lines.push(`     Error: ${row.error}`);
  }
  lines.push("");

  lines.push(`${c.bold}Recommendations${c.reset}`);
  for (const recommendation of report.recommendations) {
    lines.push(`  - ${recommendation}`);
  }

  return lines.join("\n");
}

function formatComparisonTable(rows: ComparisonRow[]): string {
  const headers = ["Mode", "ms", "results", "snips", "tokens", "expected", "top file"];
  const body = rows.map((row) => [
    row.label,
    formatMs(row.latencyMs),
    String(row.resultCount),
    typeof row.snippetCount === "number" ? String(row.snippetCount) : "-",
    typeof row.tokenCount === "number" ? String(row.tokenCount) : "-",
    row.expectedRank ? `#${row.expectedRank}` : "-",
    row.topFile ? shortenPath(row.topFile, 58) : "-",
  ]);
  return renderTable(headers, body);
}

function renderTable(headers: string[], rows: string[][]): string {
  const widths = headers.map((header, idx) => {
    return Math.max(header.length, ...rows.map((row) => row[idx]?.length || 0));
  });
  const formatRow = (row: string[]) => `  ${row.map((cell, idx) => cell.padEnd(widths[idx])).join("  ")}`;
  return [
    formatRow(headers),
    formatRow(widths.map((width) => "-".repeat(width))),
    ...rows.map(formatRow),
  ].join("\n");
}

function statusText(status: CommandStatus, c: ReturnType<typeof colors>): string {
  if (status === "ok") return `${c.green}OK${c.reset}`;
  if (status === "skipped") return `${c.yellow}SKIPPED${c.reset}`;
  return `${c.red}ERROR${c.reset}`;
}

function cardStatus(status: FeatureCard["status"], c: ReturnType<typeof colors>): string {
  if (status === "available") return `${c.green}[available]${c.reset}`;
  if (status === "degraded") return `${c.yellow}[degraded]${c.reset}`;
  return `${c.red}[missing]${c.reset}`;
}

function formatMs(ms: number): string {
  return `${Math.round(ms)}ms`;
}

function formatRankLift(delta: number): string {
  if (delta > 0) return `+${delta} positions better than baseline`;
  if (delta < 0) return `${Math.abs(delta)} positions worse than baseline`;
  return "same expected-file rank as baseline";
}

function shortenPath(path: string, max: number): string {
  if (path.length <= max) return path;
  return `...${path.slice(path.length - max + 3)}`;
}

function uniqueStrings(values: string[]): string[] {
  return [...new Set(values.filter(Boolean))].sort();
}

function colors(enabled: boolean) {
  if (!enabled) {
    return { reset: "", bold: "", dim: "", green: "", yellow: "", red: "" };
  }
  return {
    reset: "\x1b[0m",
    bold: "\x1b[1m",
    dim: "\x1b[2m",
    green: "\x1b[32m",
    yellow: "\x1b[33m",
    red: "\x1b[31m",
  };
}

async function main(): Promise<void> {
  try {
    const config = parseArgs(process.argv.slice(2));
    const report = await runShowcase(config);
    const rendered = renderReport(report, { color: config.color });
    console.log(rendered);
    if (config.jsonOutput) {
      mkdirSync(resolve(config.jsonOutput, ".."), { recursive: true });
      writeFileSync(config.jsonOutput, JSON.stringify(report, null, 2) + "\n");
    }
    if (config.markdownOutput) {
      mkdirSync(resolve(config.markdownOutput, ".."), { recursive: true });
      writeFileSync(config.markdownOutput, renderReport(report, { color: false }) + "\n");
    }
  } catch (error) {
    console.error(`aft-feature-showcase: ${error instanceof Error ? error.message : String(error)}`);
    process.exit(1);
  }
}

if (import.meta.main) {
  await main();
}
