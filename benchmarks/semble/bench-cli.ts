/**
 * Benchmark CLI parser and preflight checker.
 *
 * Provides deterministic profile/suite/mode selection with strict or degraded
 * failure modes. Normalizes legacy flag names to canonical mode names.
 */

import { existsSync, statSync } from "fs";
import { resolve } from "path";
import { getProfile, listProfiles, type BenchmarkProfile } from "./bench-profiles";
import { loadAllCanonSuites, loadCanonRepos, loadModeMatrix } from "./canon-loader";

// ---------------------------------------------------------------------------
// Canonical mode names
// ---------------------------------------------------------------------------

export const ALL_MODES = [
  "rg",
  "aft-grep",
  "fts5_search",
  "fts5_find_symbol_exact",
  "fts5_find_symbol_prefix",
  "glob",
  "ast_search",
  "semantic_m2v",
  "semantic_fe",
  "semantic_api",
  "hybrid",
  "rerank",
] as const;

export type Mode = (typeof ALL_MODES)[number];

export const ALL_SUITES = [
  "semantic_nl",
  "identifier_exact",
  "identifier_prefix",
  "path_lookup",
  "structural",
  "all",
] as const;

export type Suite = (typeof ALL_SUITES)[number];

// Legacy name normalization
const MODE_ALIASES: Record<string, Mode> = {
  "fts5": "fts5_search",
  "fts5-search": "fts5_search",
  "fts5-search-exact": "fts5_find_symbol_exact",
  "fts5-find-symbol-exact": "fts5_find_symbol_exact",
  "fts5-find-symbol-prefix": "fts5_find_symbol_prefix",
  "fts5-search-prefix": "fts5_find_symbol_prefix",
  "lexical (rg)": "rg",
  "lexical": "rg",
  "semantic-m2v": "semantic_m2v",
  "semantic-fe": "semantic_fe",
  "semantic-api": "semantic_api",
  "semantic_fastembed": "semantic_fe",
  "semantic_model2vec": "semantic_m2v",
  "ast-search": "ast_search",
};

const SUITE_ALIASES: Record<string, Suite> = {
  "semantic-nl": "semantic_nl",
  "identifier-exact": "identifier_exact",
  "identifier-prefix": "identifier_prefix",
  "path-lookup": "path_lookup",
  "structural": "structural",
};

function normalizeMode(raw: string): Mode | null {
  const lower = raw.toLowerCase().trim();
  if (lower in MODE_ALIASES) return MODE_ALIASES[lower];
  // Check canonical names
  if (ALL_MODES.includes(lower as Mode)) return lower as Mode;
  return null;
}

function normalizeSuite(raw: string): Suite | null {
  const lower = raw.toLowerCase().trim().replace(/-/g, "_");
  if (lower in SUITE_ALIASES) return SUITE_ALIASES[lower];
  if (lower === "all") return "all";
  if (ALL_SUITES.includes(lower as Suite)) return lower as Suite;
  return null;
}

function normalizeBackend(raw: string): string[] {
  const backends: string[] = [];
  for (const part of raw.split(",")) {
    const trimmed = part.trim().toLowerCase();
    if (trimmed === "both") {
      backends.push("model2vec", "fastembed");
    } else if (trimmed === "skip") {
      // skip semantic
    } else if (["model2vec", "fastembed", "semantic-api", "m2v", "fe"].includes(trimmed)) {
      if (trimmed === "m2v") backends.push("model2vec");
      else if (trimmed === "fe") backends.push("fastembed");
      else backends.push(trimmed);
    }
  }
  return [...new Set(backends)];
}

// ---------------------------------------------------------------------------
// CLI config
// ---------------------------------------------------------------------------

export interface BenchConfig {
  // Profile
  profile: BenchmarkProfile;
  profileName: string;

  // Suites
  suites: Suite[];

  // Modes
  modes: Mode[];

  // Binary
  binaryPath: string;

  // Search params
  k: number;
  candidatePool: number;
  rerankPool: number;
  repetitions: number;
  warmups: number;

  // Semantic
  backends: string[];
  semanticModel: string;
  semanticApiUrl: string;
  semanticApiModel: string;

  // Rerank
  doRerank: boolean;
  rerankModel: string;
  rerankUrl: string;

  // Behavior
  allowDegraded: boolean;
  allowSeedCanon: boolean;
  autoClone: boolean;
  verbose: boolean;

  // Output
  reportJson: string | null;
  reportJsonl: string | null;
  reportMd: string | null;

  // Cache
  cacheDir: string;

  // Legacy compat
  includeLexical: boolean;
}

function parseNumericArg(value: string, name: string, min: number, max: number): number {
  const n = parseInt(value, 10);
  if (isNaN(n) || n < min || n > max) {
    console.error(`ERROR: ${name} must be a number between ${min} and ${max}, got: ${value}`);
    process.exit(1);
  }
  return n;
}

export function parseArgs(argv: string[]): BenchConfig {
  const args = argv.slice(2);

  // Defaults
  let profileName = "quick";
  let suiteNames: string[] = ["all"];
  let modeNames: string[] = [];
  let binaryPath = "aft";
  let k = 10;
  let candidatePool = 50;
  let rerankPool = 50;
  let repetitions = 1;
  let warmups = 1;
  let backends = ["model2vec", "fastembed"];
  let semanticModel = "minishlab/potion-code-16M";
  let semanticApiUrl = "";
  let semanticApiModel = "";
  let doRerank = false;
  let rerankModel = "GTE-Reranker-Modernbert";
  let rerankUrl = "http://127.0.0.1:8090/v1/rerank";
  let allowDegraded = false;
  let allowSeedCanon = false;
  let autoClone = false;
  let verbose = false;
  let reportJson: string | null = null;
  let reportJsonl: string | null = null;
  let reportMd: string | null = null;
  let cacheDir = ".bench-cache";
  let includeLexical = true;
  let showHelp = false;

  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    switch (arg) {
      case "--profile": profileName = args[++i]; break;
      case "--suite": suiteNames = args[++i].split(","); break;
      case "--mode": modeNames = args[++i].split(","); break;
      case "--binary": binaryPath = args[++i]; break;
      case "--k": k = parseNumericArg(args[++i], "--k", 1, 1000); break;
      case "--candidate-pool": candidatePool = parseNumericArg(args[++i], "--candidate-pool", 1, 10000); break;
      case "--rerank-pool": rerankPool = parseNumericArg(args[++i], "--rerank-pool", 1, 10000); break;
      case "--repetitions": repetitions = parseNumericArg(args[++i], "--repetitions", 1, 100); break;
      case "--warmups": warmups = parseNumericArg(args[++i], "--warmups", 0, 20); break;
      case "--backend": backends = normalizeBackend(args[++i]); break;
      case "--model": semanticModel = args[++i]; break;
      case "--semantic-api-url": semanticApiUrl = args[++i]; break;
      case "--semantic-api-model": semanticApiModel = args[++i]; break;
      case "--rerank": doRerank = true; break;
      case "--rerank-model": rerankModel = args[++i]; break;
      case "--rerank-url": rerankUrl = args[++i]; break;
      case "--allow-degrade": allowDegraded = true; break;
      case "--allow-seed-canon": allowSeedCanon = true; break;
      case "--auto-clone": autoClone = true; break;
      case "--verbose": case "-v": verbose = true; break;
      case "--report-json": reportJson = args[++i]; break;
      case "--report-jsonl": reportJsonl = args[++i]; break;
      case "--report-md": reportMd = args[++i]; break;
      case "--cache-dir": cacheDir = args[++i]; break;
      case "--include-lexical": includeLexical = args[++i] !== "false"; break;
      case "--output": reportJson = args[++i]; break; // legacy alias
      case "--help": case "-h": showHelp = true; break;
      default:
        console.error(`ERROR: Unknown argument: ${arg}`);
        console.error("Run with --help for usage.");
        process.exit(1);
    }
  }

  if (showHelp) {
    printHelp();
    process.exit(0);
  }

  // Validate profile
  const profile = getProfile(profileName);
  if (!profile) {
    console.error(`ERROR: Unknown profile: ${profileName}`);
    console.error(`Available profiles: ${listProfiles().join(", ")}`);
    process.exit(1);
  }

  // Normalize suites
  const suites: Suite[] = [];
  for (const s of suiteNames) {
    const normalized = normalizeSuite(s);
    if (!normalized) {
      console.error(`ERROR: Unknown suite: ${s}`);
      console.error(`Available suites: ${ALL_SUITES.filter(s => s !== "all").join(", ")}`);
      process.exit(1);
    }
    if (normalized === "all") {
      suites.push("semantic_nl", "identifier_exact", "identifier_prefix", "path_lookup", "structural");
    } else {
      suites.push(normalized);
    }
  }

  // Normalize modes
  const modes: Mode[] = [];
  for (const m of modeNames) {
    const normalized = normalizeMode(m);
    if (!normalized) {
      console.error(`ERROR: Unknown mode: ${m}`);
      console.error(`Available modes: ${ALL_MODES.join(", ")}`);
      process.exit(1);
    }
    modes.push(normalized);
  }

  // If no modes specified, use profile defaults (empty = all eligible per suite)
  // If modes specified, use them

  return {
    profile,
    profileName,
    suites: [...new Set(suites)],
    modes: [...new Set(modes)],
    binaryPath,
    k,
    candidatePool,
    rerankPool,
    repetitions: profile.repetitions || repetitions,
    warmups: profile.warmups || warmups,
    backends,
    semanticModel,
    semanticApiUrl,
    semanticApiModel,
    doRerank,
    rerankModel,
    rerankUrl,
    allowDegraded: allowDegraded || profile.allow_seed_canon,
    allowSeedCanon: allowSeedCanon || profile.allow_seed_canon,
    autoClone,
    verbose,
    reportJson,
    reportJsonl,
    reportMd,
    cacheDir,
    includeLexical,
  };
}

function printHelp() {
  console.log(`
AFT Semble Benchmark Runner

Usage:
  bun run benchmarks/semble/pilot.ts --binary <path> [options]

Profiles:
  --profile <name>       Benchmark profile (default: quick)
                         smoke     - 2 queries/repo, reviewed only, fastest
                         quick     - all reviewed + seed, 1 repetition
                         extended  - all canon, all modes, 3 repetitions
                         manual-full - full corpus, 5 repetitions

Suites:
  --suite <list>         Comma-separated suites to run (default: all)
                         semantic_nl, identifier_exact, identifier_prefix,
                         path_lookup, structural, all

Modes:
  --mode <list>          Comma-separated modes to run (default: all eligible)
                         rg, aft-grep, fts5_search, fts5_find_symbol_exact,
                         fts5_find_symbol_prefix, glob, ast_search,
                         semantic_m2v, semantic_fe, semantic_api, hybrid, rerank

Binary & Search:
  --binary <path>        AFT binary path (default: aft)
  --k <n>                Top-k results (default: 10)
  --candidate-pool <n>   Candidate pool for reranking (default: 50)
  --rerank-pool <n>      Rerank pool size (default: 50)

Semantic:
  --backend <list>       Semantic backends: both,model2vec,fastembed,semantic-api,skip
  --model <name>         Semantic model name
  --semantic-api-url <u> OpenAI-compatible endpoint URL
  --semantic-api-model <m> Model name for API endpoint

Rerank:
  --rerank               Enable reranker pass
  --rerank-model <name>  Reranker model (default: GTE-Reranker-Modernbert)
  --rerank-url <url>     Reranker endpoint

Behavior:
  --allow-degrade        Emit unavailable rows instead of failing
  --allow-seed-canon     Allow seed-status canon rows
  --auto-clone           Auto-clone missing repos
  --repetitions <n>      Query repetitions (default: from profile)
  --warmups <n>          Warmup queries (default: from profile)

Output:
  --report-json <file>   JSON report output
  --report-jsonl <file>  JSONL attempt log output
  --report-md <file>     Markdown report output
  --verbose, -v          Per-query debug output

Legacy flags:
  --output <file>        Alias for --report-json
  --cache-dir <dir>      Repo cache directory
  --include-lexical      Include lexical queries (default: true)
`);
}

// ---------------------------------------------------------------------------
// Preflight
// ---------------------------------------------------------------------------

export interface PreflightResult {
  mode: string;
  suite: string;
  status: "available" | "unavailable" | "error";
  reason?: string;
}

export function runPreflight(config: BenchConfig, canonDir: string): PreflightResult[] {
  const results: PreflightResult[] = [];

  // Check binary
  if (config.binaryPath !== "aft") {
    try {
      statSync(config.binaryPath);
    } catch {
      for (const suite of config.suites) {
        for (const mode of config.modes.length > 0 ? config.modes : ALL_MODES as unknown as Mode[]) {
          results.push({
            mode,
            suite,
            status: "error",
            reason: `AFT binary not found: ${config.binaryPath}`,
          });
        }
      }
      return results;
    }
  }

  // Check repos
  try {
    const repos = loadCanonRepos(canonDir);
    for (const repo of repos.repos) {
      const repoDir = resolve(config.cacheDir, repo.name);
      if (!existsSync(repoDir)) {
        if (!config.autoClone) {
          results.push({
            mode: "all",
            suite: "all",
            status: "unavailable",
            reason: `Repo ${repo.name} not cloned (use --auto-clone)`,
          });
        }
      }
    }
  } catch {
    results.push({
      mode: "all",
      suite: "all",
      status: "error",
      reason: "Failed to load repos.json from canon directory",
    });
  }

  // Check mode availability per suite
  const matrix = loadModeMatrix(canonDir);
  for (const suite of config.suites) {
    const modesToCheck = config.modes.length > 0 ? config.modes : ALL_MODES as unknown as Mode[];
    for (const mode of modesToCheck) {
      // Check if mode is relevant for this suite
      const suiteEntry = matrix.suites[suite];
      const isPrimary = suiteEntry?.primary_modes.includes(mode) ?? false;
      const isControl = suiteEntry?.control_modes.includes(mode) ?? false;

      if (!isPrimary && !isControl && suiteEntry) {
        // Mode not in matrix for this suite — skip (not an error, just not applicable)
        continue;
      }

      // Check FTS5 availability
      if (mode.startsWith("fts5_")) {
        // FTS5 modes require the feature — we can't check this without running the binary
        // Mark as available with a caveat
        results.push({ mode, suite, status: "available", reason: "FTS5 availability verified at runtime" });
        continue;
      }

      // Check semantic modes
      if (mode.startsWith("semantic_")) {
        if (config.backends.length === 0) {
          results.push({ mode, suite, status: "unavailable", reason: "No semantic backends configured" });
          continue;
        }
        if (mode === "semantic_api" && !config.semanticApiUrl) {
          results.push({ mode, suite, status: "unavailable", reason: "No --semantic-api-url provided" });
          continue;
        }
        results.push({ mode, suite, status: "available" });
        continue;
      }

      // Check rerank
      if (mode === "rerank" && !config.doRerank) {
        results.push({ mode, suite, status: "unavailable", reason: "Rerank not enabled (--rerank flag)" });
        continue;
      }

      // Everything else is available
      results.push({ mode, suite, status: "available" });
    }
  }

  return results;
}

export function printPreflight(results: PreflightResult[]): void {
  console.log("\n=== Preflight ===");
  const grouped: Record<string, PreflightResult[]> = {};
  for (const r of results) {
    const key = `${r.suite}/${r.mode}`;
    if (!grouped[key]) grouped[key] = [];
    grouped[key].push(r);
  }

  for (const [key, items] of Object.entries(grouped)) {
    const status = items[0].status;
    const icon = status === "available" ? "✓" : status === "unavailable" ? "⚠" : "✗";
    const reason = items[0].reason ? ` (${items[0].reason})` : "";
    console.log(`  ${icon} ${key}${reason}`);
  }
  console.log("");
}
