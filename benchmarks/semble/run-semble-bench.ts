#!/usr/bin/env bun
/**
 * Semble-inspired benchmark runner for AFT semantic search.
 *
 * Goals of this rewrite:
 * - Declare OpenAI-compatible embedding/reranking models and endpoints once.
 * - Reuse that declaration across all OpenAI-compatible AFT profiles.
 * - Use the correct llama.cpp rerank API shape: query + documents + top_n.
 * - Avoid silently claiming reranking is disabled when it is not.
 * - Keep CLI baselines separate, and apply optional explicit external reranking only
 *   when this script has enough result text to rerank.
 * - Any profile can be augmented with an optional reranker pass via --rerank.
 *
 * Usage:
 *   bun run benchmarks/semble/run-semble-bench.ts [options]
 *
 * Options:
 *   --profile <id>              Profile to run: a,b,c,e,f (default: c)
 *   --k <n>                     Top-k for recall/MRR result collection (default: 10)
 *   --rerank                    Enable reranker pass after embedding search
 *   --rerank-candidates <n>     Number of candidates fed to reranker (default: 30)
 *   --cache-dir <dir>           Repo cache directory (default: .bench-cache)
 *   --output <file>             Output report path (default: semble-bench-report.json)
 *   --binary <path>             AFT binary path (default: auto-detect)
 *   --allow-rerank-degrade      If reranker health check fails, skip rerank pass instead of aborting
 *   --skip-health               Do not ping model endpoints before benchmark
 *   --fail-fast                 Stop after first repo/pass error
 *   --help                      Print usage
 *
 * Environment overrides for the centralized OpenAI-compatible stack:
 *   AFT_BENCH_OPENAI_SCHEME=http
 *   AFT_BENCH_OPENAI_HOST=127.0.0.1
 *   AFT_BENCH_OPENAI_API_PREFIX=/v1
 *   AFT_BENCH_EMBED_PORT=8090
 *   AFT_BENCH_EMBED_BASE_URL=http://127.0.0.1:8090/v1
 *   AFT_BENCH_EMBED_MODEL=CodeRankEmbed
 *   AFT_BENCH_RERANK_PORT=8090
 *   AFT_BENCH_RERANK_BASE_URL=http://127.0.0.1:8090/v1
 *   AFT_BENCH_RERANK_MODEL=GTE-Reranker-Modernbert
 *   AFT_BENCH_MAX_BATCH_SIZE=16
 *   AFT_BENCH_EMBED_TIMEOUT_MS=60000
 *   AFT_BENCH_RERANK_TIMEOUT_MS=30000
 *   AFT_BENCH_RERANK_MAX_CANDIDATES=30
 */

import {
  existsSync,
  readFileSync,
  readdirSync,
  writeFileSync,
} from "fs";
import { join, resolve } from "path";
import {
  spawn,
  spawnSync,
  type ChildProcess,
  type SpawnSyncReturns,
} from "child_process";

// ---------------------------------------------------------------------------
// Central OpenAI-compatible model stack declaration
// ---------------------------------------------------------------------------

function envString(name: string, fallback: string): string {
  const value = process.env[name];
  return value && value.trim() ? value.trim() : fallback;
}

function envInt(name: string, fallback: number): number {
  const raw = process.env[name];
  if (!raw || !raw.trim()) return fallback;
  const parsed = Number.parseInt(raw, 10);
  return Number.isFinite(parsed) ? parsed : fallback;
}

function trimTrailingSlash(value: string): string {
  return value.replace(/\/+$/, "");
}

function ensureLeadingSlash(value: string): string {
  return value.startsWith("/") ? value : `/${value}`;
}

function joinUrl(baseUrl: string, path: string): string {
  return `${trimTrailingSlash(baseUrl)}${ensureLeadingSlash(path)}`;
}

const OPENAI_STACK = (() => {
  const scheme = envString("AFT_BENCH_OPENAI_SCHEME", "http");
  const host = envString("AFT_BENCH_OPENAI_HOST", "127.0.0.1");
  const apiPrefix = ensureLeadingSlash(envString("AFT_BENCH_OPENAI_API_PREFIX", "/v1"));

  const embedPort = envInt("AFT_BENCH_EMBED_PORT", 8090);
  const rerankPort = envInt("AFT_BENCH_RERANK_PORT", embedPort);

  const defaultEmbedBaseUrl = `${scheme}://${host}:${embedPort}${apiPrefix}`;
  const defaultRerankBaseUrl = `${scheme}://${host}:${rerankPort}${apiPrefix}`;

  return {
    scheme,
    host,
    apiPrefix,
    embedding: {
      port: embedPort,
      baseUrl: trimTrailingSlash(envString("AFT_BENCH_EMBED_BASE_URL", defaultEmbedBaseUrl)),
      model: envString("AFT_BENCH_EMBED_MODEL", "CodeRankEmbed"),
      timeoutMs: envInt("AFT_BENCH_EMBED_TIMEOUT_MS", 60_000),
      maxBatchSize: envInt("AFT_BENCH_MAX_BATCH_SIZE", 16),
    },
    reranker: {
      port: rerankPort,
      baseUrl: trimTrailingSlash(envString("AFT_BENCH_RERANK_BASE_URL", defaultRerankBaseUrl)),
      model: envString("AFT_BENCH_RERANK_MODEL", "GTE-Reranker-Modernbert"),
      timeoutMs: envInt("AFT_BENCH_RERANK_TIMEOUT_MS", 30_000),
      maxCandidates: envInt("AFT_BENCH_RERANK_MAX_CANDIDATES", 30),
    },
  } as const;
})();

function embeddingUrl(): string {
  return joinUrl(OPENAI_STACK.embedding.baseUrl, "/embeddings");
}

function rerankUrl(): string {
  return joinUrl(OPENAI_STACK.reranker.baseUrl, "/rerank");
}

function openAiAftSemanticConfig(enableRerank: boolean, rerankMaxCandidates: number = OPENAI_STACK.reranker.maxCandidates): Record<string, unknown> {
  const config: Record<string, unknown> = {
    backend: "openai_compatible",
    base_url: OPENAI_STACK.embedding.baseUrl,
    model: OPENAI_STACK.embedding.model,
    diagnostics_enabled: true,
    max_batch_size: OPENAI_STACK.embedding.maxBatchSize,
    timeout_ms: OPENAI_STACK.embedding.timeoutMs,
    rerank_enabled: enableRerank,
  };

  if (enableRerank) {
    Object.assign(config, {
      rerank_model: OPENAI_STACK.reranker.model,
      rerank_base_url: OPENAI_STACK.reranker.baseUrl,
      rerank_timeout_ms: OPENAI_STACK.reranker.timeoutMs,
      rerank_max_candidates: rerankMaxCandidates,
      rerank_api_type: "rerank", // cross-encoder models use /v1/rerank endpoint
    });
  }

  return config;
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface Annotation {
  query: string;
  relevant: (string | { path: string; start_line?: number; end_line?: number })[];
  secondary: (string | { path: string; start_line?: number; end_line?: number })[];
  category: string;
  repo_name?: string;
}

interface Repo {
  name: string;
  language: string;
  benchmark_root: string | null;
}

interface Fixture {
  repos: Repo[];
  annotations: Annotation[];
}

interface SearchResult {
  file: string;
  score?: number;
  line?: number;
  content?: string;
}

interface BenchResult {
  mode: string;
  query: string;
  repo_name: string;
  category: string;
  latency_ms: number;
  results: Array<{ file: string; score?: number }>;
  recall_at_k: number;
  mrr: number;
}

interface BenchReport {
  timestamp: string;
  profile: string;
  profile_label: string;
  k: number;
  binary: string;
  openai_stack: {
    embedding: {
      model: string;
      base_url: string;
      port: number;
    };
    reranker: {
      model: string;
      base_url: string;
      port: number;
      max_candidates: number;
    };
  };
  results: BenchResult[];
  aggregate: Record<string, AggregateOut>;
  by_category: Record<string, Record<string, GroupOut>>;
  by_repo: Record<string, Record<string, GroupOut>>;
}

interface AggregateOut {
  recall: number;
  mrr: number;
  count: number;
  mean_latency_ms: number;
}

interface GroupOut {
  recall: number;
  mrr: number;
  count: number;
}

type ProfileMode = "aft" | "cli";
type CliKind = "semble" | "colgrep";

interface Profile {
  id: string;
  label: string;
  description: string;
  mode: ProfileMode;
  requiresEmbedding?: boolean;
  requiresFeature?: string;

  // AFT profiles — supportsRerank means the AFT binary can handle reranking internally
  supportsRerank?: boolean;
  getAftSemanticConfig?: (enableRerank: boolean, rerankMaxCandidates: number) => Record<string, unknown>;

  // CLI profiles — supportsExternalRerank means the script can apply reranking after CLI search
  cliKind?: CliKind;
  supportsExternalRerank?: boolean;
}

function localBackendAftSemanticConfig(
  backendConfig: Record<string, unknown>,
  enableRerank: boolean,
  rerankMaxCandidates: number,
): Record<string, unknown> {
  const config: Record<string, unknown> = {
    ...backendConfig,
    diagnostics_enabled: true,
    rerank_enabled: enableRerank,
  };

  if (enableRerank) {
    Object.assign(config, {
      rerank_model: OPENAI_STACK.reranker.model,
      rerank_base_url: OPENAI_STACK.reranker.baseUrl,
      rerank_timeout_ms: OPENAI_STACK.reranker.timeoutMs,
      rerank_max_candidates: rerankMaxCandidates,
      rerank_api_type: "rerank",
    });
  }

  return config;
}

const PROFILES: Record<string, Profile> = {
  a: {
    id: "a",
    label: "fastembed",
    description: "AFT fastembed backend — all-MiniLM-L6-v2",
    mode: "aft",
    supportsRerank: true,
    getAftSemanticConfig: (enableRerank: boolean, rerankMaxCandidates: number) =>
      localBackendAftSemanticConfig(
        { backend: "fastembed", model: "all-MiniLM-L6-v2" },
        enableRerank,
        rerankMaxCandidates,
      ),
  },
  b: {
    id: "b",
    label: "model2vec",
    description: "AFT model2vec backend — Potion Code 16M [requires semantic-model2vec feature]",
    mode: "aft",
    supportsRerank: true,
    getAftSemanticConfig: (enableRerank: boolean, rerankMaxCandidates: number) =>
      localBackendAftSemanticConfig(
        {
          backend: "model2vec",
          model: "minishlab/potion-code-16M",
          model_path: envString("AFT_BENCH_MODEL2VEC_PATH", "D:/AI/LLM_models/potion-code-16M"),
        },
        enableRerank,
        rerankMaxCandidates,
      ),
    requiresFeature: "semantic-model2vec",
  },
  c: {
    id: "c",
    label: "openai-embed",
    description: `AFT OpenAI-compatible embedding — ${OPENAI_STACK.embedding.model} @ ${OPENAI_STACK.embedding.baseUrl}`,
    mode: "aft",
    requiresEmbedding: true,
    supportsRerank: true,
    getAftSemanticConfig: (enableRerank: boolean, rerankMaxCandidates: number) =>
      openAiAftSemanticConfig(enableRerank, rerankMaxCandidates),
  },
  e: {
    id: "e",
    label: "semble",
    description: "Semble CLI baseline — no centralized OpenAI embedding injected by this script",
    mode: "cli",
    cliKind: "semble",
    supportsExternalRerank: true,
  },
  f: {
    id: "f",
    label: "colgrep",
    description: "colgrep CLI baseline — no centralized OpenAI embedding injected by this script",
    mode: "cli",
    cliKind: "colgrep",
    supportsExternalRerank: true,
  },
};

// ---------------------------------------------------------------------------
// CLI argument parsing
// ---------------------------------------------------------------------------

interface Options {
  k: number;
  cacheDir: string;
  outputFile: string;
  binaryPath: string;
  profileId: string;
  allowRerankDegrade: boolean;
  skipHealth: boolean;
  failFast: boolean;
  rerank: boolean;
  rerankCandidates: number;
}

function printUsage(): void {
  console.log(`Usage:
  bun run benchmarks/semble/run-semble-bench.ts [options]

Options:
  --profile <id>              Profile to run: ${Object.keys(PROFILES).join(",")} (default: c)
  --k <n>                     Top-k for recall/MRR result collection (default: 10)
  --rerank                    Enable reranker pass after embedding search
  --rerank-candidates <n>     Number of candidates fed to reranker (default: ${OPENAI_STACK.reranker.maxCandidates})
  --cache-dir <dir>           Repo cache directory (default: .bench-cache)
  --output <file>             Output report path (default: semble-bench-report.json)
  --binary <path>             AFT binary path (default: auto-detect)
  --allow-rerank-degrade      If reranker health fails, skip rerank pass instead of aborting
  --skip-health               Do not ping model endpoints before benchmark
  --fail-fast                 Stop after first repo/pass error
  --help                      Print this help

Central OpenAI-compatible stack:
  embedding model: ${OPENAI_STACK.embedding.model}
  embedding URL:   ${embeddingUrl()}
  reranker model:  ${OPENAI_STACK.reranker.model}
  rerank URL:      ${rerankUrl()}

Reranker behavior:
  When --rerank is enabled, the benchmark fetches --rerank-candidates results
  from the embedding model, feeds them to the reranker, and evaluates recall
  on the top --k reranked results. Without --rerank, only --k results are
  fetched and evaluated directly.
`);
}

function parseArgs(argv: string[]): Options {
  const opts: Options = {
    k: 10,
    cacheDir: ".bench-cache",
    outputFile: "semble-bench-report.json",
    binaryPath: "",
    profileId: "c",
    allowRerankDegrade: false,
    skipHealth: false,
    failFast: false,
    rerank: false,
    rerankCandidates: OPENAI_STACK.reranker.maxCandidates,
  };

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    switch (arg) {
      case "--k":
        opts.k = parsePositiveInt(argv[++i], "--k");
        break;
      case "--cache-dir":
        opts.cacheDir = requireValue(argv[++i], "--cache-dir");
        break;
      case "--output":
        opts.outputFile = requireValue(argv[++i], "--output");
        break;
      case "--binary":
        opts.binaryPath = requireValue(argv[++i], "--binary");
        break;
      case "--profile":
        opts.profileId = requireValue(argv[++i], "--profile");
        break;
      case "--rerank":
        opts.rerank = true;
        break;
      case "--rerank-candidates":
        opts.rerankCandidates = parsePositiveInt(argv[++i], "--rerank-candidates");
        break;
      case "--allow-rerank-degrade":
        opts.allowRerankDegrade = true;
        break;
      case "--skip-health":
        opts.skipHealth = true;
        break;
      case "--fail-fast":
        opts.failFast = true;
        break;
      case "--help":
      case "-h":
        printUsage();
        process.exit(0);
      default:
        throw new Error(`Unknown argument: ${arg}`);
    }
  }

  return opts;
}

function requireValue(value: string | undefined, flag: string): string {
  if (!value || value.startsWith("--")) {
    throw new Error(`${flag} requires a value`);
  }
  return value;
}

function parsePositiveInt(value: string | undefined, flag: string): number {
  const raw = requireValue(value, flag);
  const parsed = Number.parseInt(raw, 10);
  if (!Number.isFinite(parsed) || parsed <= 0) {
    throw new Error(`${flag} must be a positive integer, got: ${raw}`);
  }
  return parsed;
}

// ---------------------------------------------------------------------------
// Scoring helpers
// ---------------------------------------------------------------------------

function normalizePath(p: string): string {
  return String(p ?? "")
    .replace(/^\\\\\?\\/, "")
    .replace(/\\/g, "/")
    .replace(/^\.\//, "")
    .toLowerCase();
}

function normalizeAnnotationPath(r: string | { path: string; [key: string]: unknown }): string {
  return typeof r === "string" ? r : r.path;
}

function buildRelevantPaths(ann: Annotation): string[] {
  const all = [...(ann.relevant || []), ...(ann.secondary || [])];
  return all.map((entry) => normalizeAnnotationPath(entry)).filter(Boolean);
}

function pathMatches(a: string, b: string): boolean {
  const na = normalizePath(a);
  const nb = normalizePath(b);
  return Boolean(na && nb && (na === nb || na.endsWith(`/${nb}`) || nb.endsWith(`/${na}`)));
}

function recallAtK(retrieved: Array<{ file: string }>, relevant: string[], k: number): number {
  if (relevant.length === 0) return 0;
  const retrievedK = retrieved.slice(0, k);
  let hits = 0;
  for (const rel of relevant) {
    if (retrievedK.some((r) => pathMatches(r.file, rel))) hits++;
  }
  return hits / relevant.length;
}

function mrr(retrieved: Array<{ file: string }>, relevant: string[]): number {
  for (let i = 0; i < retrieved.length; i++) {
    if (relevant.some((rel) => pathMatches(retrieved[i].file, rel))) return 1 / (i + 1);
  }
  return 0;
}

// ---------------------------------------------------------------------------
// AFT binary interaction via NDJSON
// ---------------------------------------------------------------------------

function findBinary(): string {
  const localAppData = process.env.LOCALAPPDATA || "";
  const home = process.env.HOME || process.env.USERPROFILE || "";
  const candidates: string[] = [
    resolve("crates/aft/target/release/aft.exe"),
    resolve("crates/aft/target/release/aft"),
    ...(localAppData ? [join(localAppData, "aft/bin/aft.exe")] : []),
    join(home, ".cache/aft/bin/aft"),
  ];

  if (localAppData) {
    const binDir = join(localAppData, "aft/bin");
    if (existsSync(binDir)) {
      for (const v of readdirSync(binDir)) {
        const exe = join(binDir, v, "aft.exe");
        if (existsSync(exe)) candidates.push(exe);
      }
    }
  }

  for (const candidate of candidates) {
    if (existsSync(candidate)) return candidate;
  }
  throw new Error("AFT binary not found. Build with cargo or pass --binary.");
}

interface BridgeResponse {
  id?: string;
  success: boolean;
  data?: Record<string, unknown>;
  text?: string;
  message?: string;
  code?: string;
  status?: string;
  results?: unknown[];
  [key: string]: unknown;
}

const DEFAULT_READY_PROBES = [
  "function",
  "request",
  "route",
  "handler",
  "class",
  "module",
  "config",
  "schema",
  "test",
];

function buildReadyProbeQueries(annotations: Array<Annotation & { repo_name: string }>): string[] {
  const probes = new Set<string>(DEFAULT_READY_PROBES);
  for (const ann of annotations.slice(0, 8)) {
    const query = ann.query.trim();
    if (query) probes.add(query);
  }
  return [...probes];
}

function isFatalEmbeddingServerMessage(message: string): boolean {
  const msg = message.toLowerCase();
  return (
    msg.includes("exceed_context_size_error") ||
    msg.includes("larger than the max context size") ||
    msg.includes("too large to process") ||
    (msg.includes("input") && msg.includes("too large") && msg.includes("batch size"))
  );
}

function explainFatalEmbeddingServerMessage(message: string): string {
  return (
    `Embedding server rejected an indexing/search input: ${message}
` +
    `Likely fixes: restart llama-swap so the intended --ctx-size is active; ` +
    `raise CodeRankEmbed --ctx-size/--batch-size/--ubatch-size; and add/correct AFT chunking so one huge symbol/file is not embedded as a single request.`
  );
}

class AftBridge {
  private proc: ChildProcess;
  private pending = new Map<string, {
    resolve: (v: BridgeResponse) => void;
    reject: (e: Error) => void;
    timeout: ReturnType<typeof setTimeout>;
  }>();
  private buffer = "";
  private stderrLines: string[] = [];
  private label: string;
  private searchFailCount = 0;

  constructor(binaryPath: string, projectRoot: string, label = "") {
    this.label = label || projectRoot.split(/[\\/]/).pop() || "unknown";

    this.proc = spawn(binaryPath, [], {
      stdio: ["pipe", "pipe", "pipe"],
      env: { ...process.env, RUST_LOG: process.env.RUST_LOG || "info" },
    });

    this.proc.stdout!.on("data", (chunk: Buffer) => this.onStdout(chunk));
    this.proc.stderr!.on("data", (chunk: Buffer) => this.onStderr(chunk));
    this.proc.on("error", (err) => this.rejectAll(err));
    this.proc.on("exit", (code, signal) => {
      if (this.pending.size > 0) {
        this.rejectAll(new Error(`AFT exited while requests were pending: code=${code}, signal=${signal}`));
      }
    });
  }

  private onStdout(chunk: Buffer): void {
    this.buffer += chunk.toString();
    const lines = this.buffer.split("\n");
    this.buffer = lines.pop() || "";

    for (const line of lines) {
      const trimmed = line.trim();
      if (!trimmed) continue;

      try {
        const msg = JSON.parse(trimmed) as BridgeResponse;
        if (!msg.id) continue;
        const pending = this.pending.get(msg.id);
        if (!pending) continue;
        clearTimeout(pending.timeout);
        this.pending.delete(msg.id);
        pending.resolve(msg);
      } catch {
        // Ignore non-NDJSON logs on stdout.
      }
    }
  }

  private onStderr(chunk: Buffer): void {
    for (const line of chunk.toString().split("\n")) {
      const trimmed = line.trim();
      if (!trimmed) continue;
      this.stderrLines.push(trimmed);
      if (this.stderrLines.length > 200) this.stderrLines.shift();
    }
  }

  private rejectAll(err: Error): void {
    for (const [id, pending] of this.pending.entries()) {
      clearTimeout(pending.timeout);
      pending.reject(err);
      this.pending.delete(id);
    }
  }

  async send(command: string, params: Record<string, unknown>, timeoutMs = 60_000): Promise<BridgeResponse> {
    if (!this.proc.stdin || this.proc.stdin.destroyed) {
      throw new Error(`AFT stdin is closed for ${this.label}`);
    }

    const id = `${command}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    const request = { id, command, ...params };

    return new Promise<BridgeResponse>((resolve, reject) => {
      const timeout = setTimeout(() => {
        if (!this.pending.has(id)) return;
        this.pending.delete(id);
        const tail = this.getStderrTail(20).join("\n");
        reject(new Error(`Timeout on ${command} for ${this.label}${tail ? `\nStderr tail:\n${tail}` : ""}`));
      }, timeoutMs);

      this.pending.set(id, { resolve, reject, timeout });
      this.proc.stdin!.write(`${JSON.stringify(request)}\n`);
    });
  }

  async configure(projectRoot: string, semanticPayload: Record<string, unknown>): Promise<BridgeResponse> {
    const resp = await this.send("configure", {
      project_root: projectRoot,
      harness: "opencode",
      semantic_search: true,
      search_index: true,
      semantic: semanticPayload,
    });

    if (!resp.success) {
      throw new Error(
        `Configure failed for ${this.label}: ${resp.message ?? resp.code ?? resp.text ?? "unknown"}\n` +
        stderrBlock(this.getStderrTail(20)),
      );
    }

    return resp;
  }

  getStderrTail(n = 20): string[] {
    return this.stderrLines.slice(-n);
  }

  async waitReady(probeQueries: string[] = DEFAULT_READY_PROBES): Promise<void> {
    const probes = probeQueries.length > 0 ? probeQueries : DEFAULT_READY_PROBES;
    let consecutiveEmptyResultPolls = 0;
    let consecutiveNonBuildingNoResultPolls = 0;

    for (let i = 0; i < 300; i++) {
      const probeQuery = probes[i % probes.length];
      try {
        const resp = await this.send("semantic_search", { query: probeQuery, top_k: 3 });
        const status = String(resp.status || "");

        if (status === "building") {
          consecutiveEmptyResultPolls = 0;
          consecutiveNonBuildingNoResultPolls = 0;
          if (i % 5 === 0) console.log(`      [poll ${i}] building (${String(resp.stage || "?")})`);
        } else if (resp.success && Array.isArray(resp.results)) {
          if (resp.results.length > 0) return;

          consecutiveEmptyResultPolls++;
          if (i % 5 === 0) {
            console.log(`      [poll ${i}] ready probe returned 0 results for "${truncate(probeQuery, 80)}"; waiting`);
          }

          // Some projects legitimately do not match the generic probes, and AFT
          // has no dedicated readiness command here. Do not return immediately,
          // because that masks empty-index/parser failures such as Express.
          if (consecutiveEmptyResultPolls >= Math.max(15, Math.min(30, probes.length * 2))) {
            console.warn(
              `      [poll ${i}] assuming index is ready after repeated successful empty probes. ` +
              `If this repo later shows 0 results, suspect empty/unsupported indexing or wrong benchmark root.`,
            );
            return;
          }
        } else if (!resp.success) {
          const msg = String(resp.message ?? resp.code ?? resp.text ?? "unknown");
          if (isFatalEmbeddingServerMessage(msg)) {
            throw new Error(explainFatalEmbeddingServerMessage(msg));
          }
          if (i % 5 === 0) console.log(`      [poll ${i}] probe: ${msg}`);
        } else {
          const text = String(resp.text || "");
          if (text.includes("building")) {
            consecutiveNonBuildingNoResultPolls = 0;
            if (i % 5 === 0) console.log(`      [poll ${i}] building (via text)`);
          } else {
            consecutiveNonBuildingNoResultPolls++;
            if (i % 5 === 0) {
              console.log(`      [poll ${i}] non-building response without results; waiting`);
            }
            if (consecutiveNonBuildingNoResultPolls >= 5) {
              console.warn(`      [poll ${i}] assuming index is ready after repeated non-building responses without results.`);
              return;
            }
          }
        }
      } catch (err) {
        if (String(err).includes("Embedding server rejected an indexing/search input")) throw err;
        if (i % 5 === 0) {
          console.log(`      [poll ${i}] error: ${err}`);
          const tail = this.getStderrTail(5);
          if (tail.length) console.log(`      stderr: ${tail.join("\n      stderr: ")}`);
        }
      }
      await sleep(2_000);
    }

    throw new Error(`Timeout waiting for AFT to be ready (${this.label})\n${stderrBlock(this.getStderrTail(30))}`);
  }

  async search(query: string, topK: number): Promise<{ results: SearchResult[]; latency_ms: number }> {
    const start = performance.now();
    const resp = await this.send("semantic_search", { query, top_k: topK });
    const latency_ms = performance.now() - start;

    if (!resp.success) {
      if (this.searchFailCount < 3) {
        console.log(`      [search] FAIL (${latency_ms.toFixed(0)}ms): ${resp.message ?? resp.code ?? resp.text ?? "unknown"}`);
        this.searchFailCount++;
      }
      return { results: [], latency_ms };
    }

    if (!Array.isArray(resp.results)) {
      if (this.searchFailCount < 3) {
        console.log(`      [search] NO RESULTS ARRAY (${latency_ms.toFixed(0)}ms): status=${resp.status} keys=[${Object.keys(resp).join(",")}]`);
        this.searchFailCount++;
      }
      return { results: [], latency_ms };
    }

    return {
      results: resp.results.map((raw) => normalizeResult(raw as Record<string, unknown>)),
      latency_ms,
    };
  }

  shutdown(): void {
    for (const pending of this.pending.values()) clearTimeout(pending.timeout);
    this.pending.clear();
    if (!this.proc.killed) this.proc.kill();
  }
}

function normalizeResult(r: Record<string, unknown>): SearchResult {
  const file = String(r.file ?? r.path ?? r.file_path ?? r.filename ?? "");
  const scoreRaw = r.score ?? r.similarity ?? r.relevance_score;
  const lineRaw = r.line ?? r.start_line;
  return {
    file,
    score: typeof scoreRaw === "number" ? scoreRaw : Number.isFinite(Number(scoreRaw)) ? Number(scoreRaw) : undefined,
    line: typeof lineRaw === "number" ? lineRaw : Number.isFinite(Number(lineRaw)) ? Number(lineRaw) : undefined,
    content: typeof r.content === "string" ? r.content : typeof r.text === "string" ? r.text : undefined,
  };
}

function stderrBlock(lines: string[]): string {
  return lines.length ? `Stderr tail:\n${lines.join("\n")}` : "No stderr captured.";
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

// ---------------------------------------------------------------------------
// OpenAI-compatible health checks and explicit reranking
// ---------------------------------------------------------------------------

interface HealthResult {
  ok: boolean;
  status?: number;
  body?: string;
  error?: string;
}

async function pingEmbedding(): Promise<HealthResult> {
  try {
    const resp = await fetch(embeddingUrl(), {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        model: OPENAI_STACK.embedding.model,
        input: "test connectivity",
      }),
      signal: AbortSignal.timeout(15_000),
    });

    if (resp.ok) return { ok: true, status: resp.status };
    return { ok: false, status: resp.status, body: await safeText(resp) };
  } catch (err) {
    return { ok: false, error: String(err) };
  }
}

async function pingReranker(): Promise<HealthResult> {
  try {
    const resp = await fetch(rerankUrl(), {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        model: OPENAI_STACK.reranker.model,
        query: "test connectivity",
        documents: [
          "irrelevant text",
          "test connectivity document",
          "another irrelevant text",
        ],
        top_n: 1,
      }),
      signal: AbortSignal.timeout(15_000),
    });

    if (resp.ok) return { ok: true, status: resp.status };
    return { ok: false, status: resp.status, body: await safeText(resp) };
  } catch (err) {
    return { ok: false, error: String(err) };
  }
}

async function safeText(resp: Response): Promise<string> {
  try {
    return await resp.text();
  } catch {
    return "<failed to read response body>";
  }
}

function formatHealthFailure(result: HealthResult): string {
  if (result.status) return `HTTP ${result.status}${result.body ? `: ${truncate(result.body, 600)}` : ""}`;
  return result.error || "unknown error";
}

async function checkProfileHealth(profile: Profile, opts: Options): Promise<{ rerankAvailable: boolean }> {
  if (opts.skipHealth) {
    console.log("--- Model connectivity check skipped ---\n");
    return { rerankAvailable: true };
  }

  console.log("\n--- Model connectivity check ---");
  let rerankAvailable = true;

  if (profile.requiresEmbedding) {
    const embedding = await pingEmbedding();
    if (embedding.ok) {
      console.log(`  Embedding (${OPENAI_STACK.embedding.model} @ ${embeddingUrl()}): ✓ reachable`);
    } else {
      throw new Error(`Embedding health check failed: ${formatHealthFailure(embedding)}`);
    }
  }

  if (opts.rerank && (profile.supportsRerank || profile.supportsExternalRerank)) {
    const reranker = await pingReranker();
    if (reranker.ok) {
      console.log(`  Reranker  (${OPENAI_STACK.reranker.model} @ ${rerankUrl()}): ✓ reachable`);
    } else if (opts.allowRerankDegrade) {
      rerankAvailable = false;
      console.warn(`  Reranker  (${OPENAI_STACK.reranker.model} @ ${rerankUrl()}): ✗ ${formatHealthFailure(reranker)}`);
      console.warn("  Rerank degradation allowed: rerank pass will be skipped.");
    } else {
      throw new Error(`Reranker health check failed: ${formatHealthFailure(reranker)}`);
    }
  }

  console.log("");
  return { rerankAvailable };
}

async function rerankResults(query: string, results: SearchResult[], topN: number, maxCandidates: number, repoDir: string): Promise<SearchResult[]> {
  const candidates = results.slice(0, Math.min(results.length, maxCandidates));
  if (candidates.length <= 1) return results;

  const documents = candidates.map((result) => result.content || readCandidateContent(repoDir, result.file));
  const start = performance.now();

  const resp = await fetch(rerankUrl(), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      model: OPENAI_STACK.reranker.model,
      query,
      documents,
      top_n: Math.min(topN, candidates.length),
    }),
    signal: AbortSignal.timeout(OPENAI_STACK.reranker.timeoutMs),
  });

  if (!resp.ok) {
    throw new Error(`External rerank failed: HTTP ${resp.status}: ${truncate(await safeText(resp), 600)}`);
  }

  const json = await resp.json().catch((err) => {
    throw new Error(`External rerank returned non-JSON response: ${err}`);
  });

  const ranked = parseRerankResponse(json, candidates);
  const rankedKeys = new Set(ranked.map((r) => normalizePath(r.file)));
  const tail = results.filter((r) => !rankedKeys.has(normalizePath(r.file)));
  const latency = performance.now() - start;
  if (process.env.AFT_BENCH_VERBOSE_RERANK === "1") {
    console.log(`      [external-rerank] ${candidates.length} candidates -> ${ranked.length} ranked in ${latency.toFixed(0)}ms`);
  }

  return [...ranked, ...tail];
}

function parseRerankResponse(json: unknown, candidates: SearchResult[]): SearchResult[] {
  const root = json as Record<string, unknown>;
  const rawItems = Array.isArray(root.results)
    ? root.results
    : Array.isArray(root.data)
      ? root.data
      : [];

  const ranked: SearchResult[] = [];
  for (let position = 0; position < rawItems.length; position++) {
    const item = rawItems[position] as Record<string, unknown>;
    const indexRaw = item.index ?? (item.document as Record<string, unknown> | undefined)?.index ?? position;
    const index = Number(indexRaw);
    if (!Number.isInteger(index) || index < 0 || index >= candidates.length) continue;

    const scoreRaw = item.relevance_score ?? item.score ?? item.rank_score;
    const score = Number.isFinite(Number(scoreRaw)) ? Number(scoreRaw) : candidates[index].score;
    ranked.push({ ...candidates[index], score });
  }

  return ranked.length ? ranked : candidates;
}

function readCandidateContent(repoDir: string, file: string): string {
  const normalized = file.replace(/^\\\\\?\\/, "");
  const maybeAbsolute = /^[A-Za-z]:[\\/]/.test(normalized) || normalized.startsWith("/");
  const path = maybeAbsolute ? normalized : join(repoDir, normalized);
  try {
    const content = readFileSync(path, "utf-8");
    return content.length > 20_000 ? content.slice(0, 20_000) : content;
  } catch {
    return normalized;
  }
}

function truncate(value: string, max: number): string {
  return value.length <= max ? value : `${value.slice(0, max)}…`;
}

// ---------------------------------------------------------------------------
// CLI-based search
// ---------------------------------------------------------------------------

function runCommand(command: string, args: string[], timeoutMs: number): SpawnSyncReturns<string> {
  return spawnSync(command, args, {
    encoding: "utf-8",
    stdio: "pipe",
    timeout: timeoutMs,
    windowsHide: true,
  });
}

function sembleSearch(query: string, searchDir: string, benchmarkRoot: string | null, k: number): { results: SearchResult[]; latency_ms: number } {
  const targetDir = benchmarkRoot ? join(searchDir, benchmarkRoot) : searchDir;
  const start = performance.now();
  const out = runCommand("semble", ["search", "--top-k", String(k), query, targetDir, "--content", "all"], 30_000);

  if (out.error || out.status !== 0) {
    return { results: [], latency_ms: performance.now() - start };
  }

  const stdout = String(out.stdout || "").trim();
  return { results: parseSembleOutput(stdout, k), latency_ms: performance.now() - start };
}

function parseSembleOutput(output: string, k: number): SearchResult[] {
  if (!output) return [];
  try {
    const parsed = JSON.parse(output);
    const items = Array.isArray(parsed) ? parsed : Array.isArray(parsed.results) ? parsed.results : [];
    return items.slice(0, k).map((raw: Record<string, unknown>) => {
      const chunk = (raw.chunk ?? raw) as Record<string, unknown>;
      return {
        file: String(chunk.file_path ?? chunk.file ?? raw.file_path ?? raw.file ?? ""),
        score: typeof raw.score === "number" ? raw.score : undefined,
        line: typeof chunk.start_line === "number" ? chunk.start_line : typeof raw.line === "number" ? raw.line : undefined,
        content: typeof chunk.content === "string"
          ? chunk.content
          : typeof chunk.text === "string"
            ? chunk.text
            : typeof raw.content === "string"
              ? raw.content
              : undefined,
      };
    }).filter((r: SearchResult) => r.file);
  } catch {
    return output.split("\n").filter(Boolean).slice(0, k).map((line) => ({ file: line.trim() }));
  }
}

function colgrepSearch(query: string, searchDir: string, benchmarkRoot: string | null, k: number): { results: SearchResult[]; latency_ms: number } {
  const targetDir = benchmarkRoot ? join(searchDir, benchmarkRoot) : searchDir;
  const start = performance.now();
  const out = runCommand("colgrep", ["--json", query, "--results", String(k), "--color", "never", targetDir], 30_000);

  if (out.error || out.status !== 0) {
    return { results: [], latency_ms: performance.now() - start };
  }

  const stdout = String(out.stdout || "").trim();
  return { results: parseColgrepOutput(stdout, k), latency_ms: performance.now() - start };
}

function parseColgrepOutput(output: string, k: number): SearchResult[] {
  if (!output) return [];
  try {
    const parsed = JSON.parse(output);
    const items = Array.isArray(parsed) ? parsed : [];
    return items.slice(0, k).map((raw: Record<string, unknown>) => {
      const unit = (raw.unit ?? raw) as Record<string, unknown>;
      return {
        file: String(unit.file ?? unit.file_path ?? raw.file ?? "").replace(/^\\\\\?\\/, ""),
        score: typeof raw.score === "number" ? raw.score : undefined,
        line: typeof unit.line === "number" ? unit.line : undefined,
        content: typeof unit.content === "string"
          ? unit.content
          : typeof unit.text === "string"
            ? unit.text
            : undefined,
      };
    }).filter((r: SearchResult) => r.file);
  } catch {
    return output.split("\n").filter(Boolean).slice(0, k).map((line) => {
      const match = line.match(/^(.+):(\d+)-(\d+)$/);
      return match ? { file: match[1].trim(), line: Number.parseInt(match[2], 10) } : { file: line.trim() };
    });
  }
}

function findCliBinary(name: string): string | null {
  const lookupCommand = process.platform === "win32" ? "where" : "which";
  const result = runCommand(lookupCommand, [name], 5_000);
  if (result.error || result.status !== 0) return null;
  const first = String(result.stdout || "").split(/\r?\n/).map((s) => s.trim()).find(Boolean);
  return first || null;
}

// ---------------------------------------------------------------------------
// Reporting helpers
// ---------------------------------------------------------------------------

function printRepoRows(repoName: string, profileLabel: string, repoResults: BenchResult[]): void {
  const byMode = groupBenchResults(repoResults);
  for (const [mode, rows] of Object.entries(byMode)) {
    const agg = aggregateRows(rows);
    console.log(
      `  ${repoName.padEnd(12)} ${profileLabel.padEnd(24)} ${mode.padEnd(18)} ${String(agg.count).padStart(3)} queries` +
      `  recall=${(agg.recall * 100).toFixed(1).padStart(5)}%  mrr=${agg.mrr.toFixed(3)}` +
      `  latency=${agg.mean_latency_ms.toFixed(0).padStart(5)}ms`,
    );
  }
}

function printSummaryTable(aggregate: Record<string, AggregateOut>, k: number, profileLabel: string): void {
  console.log("\n┌────────────────────────────────────────────────────────────────────────────┐");
  console.log(`│  Semble Benchmark Summary (k=${k}, profile=${profileLabel})`);
  console.log("├────────────────────────────────────────────────────────────────────────────┤");

  if (Object.keys(aggregate).length === 0) {
    console.log("│  No successful benchmark results.".padEnd(77) + "│");
  }

  for (const [mode, data] of Object.entries(aggregate)) {
    const label = mode.padEnd(22);
    const recallStr = (data.recall * 100).toFixed(1).padStart(5);
    const mrrStr = data.mrr.toFixed(3);
    const latStr = data.mean_latency_ms.toFixed(0).padStart(5);
    console.log(`│  ${label} recall=${recallStr}%  mrr=${mrrStr}  latency=${latStr}ms  (${data.count} queries)`);
  }
  console.log("└────────────────────────────────────────────────────────────────────────────┘");
}

function groupBenchResults(rows: BenchResult[]): Record<string, BenchResult[]> {
  const grouped: Record<string, BenchResult[]> = {};
  for (const row of rows) {
    if (!grouped[row.mode]) grouped[row.mode] = [];
    grouped[row.mode].push(row);
  }
  return grouped;
}

function aggregateRows(rows: BenchResult[]): AggregateOut {
  const n = rows.length;
  if (n === 0) return { recall: 0, mrr: 0, count: 0, mean_latency_ms: 0 };
  return {
    recall: rows.reduce((sum, row) => sum + row.recall_at_k, 0) / n,
    mrr: rows.reduce((sum, row) => sum + row.mrr, 0) / n,
    count: n,
    mean_latency_ms: rows.reduce((sum, row) => sum + row.latency_ms, 0) / n,
  };
}

function aggregateByMode(rows: BenchResult[]): Record<string, AggregateOut> {
  const out: Record<string, AggregateOut> = {};
  for (const [mode, group] of Object.entries(groupBenchResults(rows))) {
    out[mode] = aggregateRows(group);
  }
  return out;
}

function aggregateNested(rows: BenchResult[], key: "category" | "repo_name"): Record<string, Record<string, GroupOut>> {
  const groups: Record<string, Record<string, BenchResult[]>> = {};
  for (const row of rows) {
    const outer = key === "category" ? row.category : row.repo_name;
    if (!groups[outer]) groups[outer] = {};
    if (!groups[outer][row.mode]) groups[outer][row.mode] = [];
    groups[outer][row.mode].push(row);
  }

  const out: Record<string, Record<string, GroupOut>> = {};
  for (const [outer, modes] of Object.entries(groups)) {
    out[outer] = {};
    for (const [mode, group] of Object.entries(modes)) {
      const agg = aggregateRows(group);
      out[outer][mode] = { recall: agg.recall, mrr: agg.mrr, count: agg.count };
    }
  }
  return out;
}

// ---------------------------------------------------------------------------
// Main benchmark execution
// ---------------------------------------------------------------------------

function repoSearchRoot(repoDir: string, repo: Repo): string {
  return repo.benchmark_root ? join(repoDir, repo.benchmark_root) : repoDir;
}

function repoRootLabel(repoDir: string, repo: Repo): string {
  return repo.benchmark_root ? `${repo.benchmark_root} => ${repoSearchRoot(repoDir, repo)}` : `.`;
}

async function runAftPass(params: {
  binaryPath: string;
  repo: Repo;
  repoDir: string;
  annotations: Array<Annotation & { repo_name: string }>;
  semanticConfig: Record<string, unknown>;
  passLabel: string;
  modeLabel: string;
  k: number;
}): Promise<BenchResult[]> {
  const { binaryPath, repo, repoDir, annotations, semanticConfig, passLabel, modeLabel, k } = params;
  const bridge = new AftBridge(binaryPath, repoDir, `${repo.name}/${passLabel}`);
  const rows: BenchResult[] = [];

  try {
    console.log(`    ${repo.name} (${passLabel}): configuring...`);
    await bridge.configure(repoDir, semanticConfig);
    console.log(`    ${repo.name} (${passLabel}): configured ✓`);

    await bridge.waitReady(buildReadyProbeQueries(annotations));
    console.log(`    ${repo.name} (${passLabel}): index ready ✓`);

    let emptySearches = 0;
    for (const ann of annotations) {
      const relevant = buildRelevantPaths(ann);
      const { results, latency_ms } = await bridge.search(ann.query, k);
      if (results.length === 0) {
        emptySearches++;
        if (emptySearches <= 3) {
          console.warn(`      [search] EMPTY RESULTS for query: ${truncate(ann.query, 120)}`);
        }
      }
      rows.push(makeBenchResult(modeLabel, ann, repo.name, results, relevant, latency_ms, k));
    }

    if (rows.length > 0 && rows.every((row) => row.results.length === 0)) {
      console.warn(
        `    ${repo.name} (${passLabel}): WARNING — every benchmark query returned 0 results. ` +
        `Likely causes: wrong AFT binary, wrong benchmark root, unsupported parser/language, or empty index.`,
      );
    }
  } finally {
    bridge.shutdown();
  }

  return rows;
}

function makeBenchResult(
  mode: string,
  ann: Annotation,
  repoName: string,
  results: SearchResult[],
  relevant: string[],
  latencyMs: number,
  k: number,
): BenchResult {
  return {
    mode,
    query: ann.query,
    repo_name: repoName,
    category: ann.category,
    latency_ms: latencyMs,
    results: results.map((r) => ({ file: r.file, score: r.score })),
    recall_at_k: recallAtK(results, relevant, k),
    mrr: mrr(results, relevant),
  };
}

async function main(): Promise<void> {
  const opts = parseArgs(process.argv.slice(2));
  const profile = PROFILES[opts.profileId];
  if (!profile) {
    throw new Error(`Unknown profile "${opts.profileId}". Available profiles: ${Object.keys(PROFILES).join(", ")}`);
  }

  console.log(`Using profile ${profile.id}: ${profile.description}`);
  console.log(`Central embedding: ${OPENAI_STACK.embedding.model} @ ${embeddingUrl()}`);
  console.log(`Central reranker:  ${OPENAI_STACK.reranker.model} @ ${rerankUrl()}`);

  // Validate rerank options
  if (opts.rerank) {
    if (!profile.supportsRerank && !profile.supportsExternalRerank) {
      throw new Error(`Profile "${profile.id}" does not support reranking. Use profiles c, e, or f with --rerank.`);
    }
    if (opts.rerankCandidates < opts.k) {
      console.warn(`  WARNING: --rerank-candidates (${opts.rerankCandidates}) < --k (${opts.k}). Adjusting to ${opts.k}.`);
      opts.rerankCandidates = opts.k;
    }
    console.log(`  Rerank: enabled (${opts.rerankCandidates} candidates → top ${opts.k})`);
  }

  const health = await checkProfileHealth(profile, opts);

  let binaryPath = opts.binaryPath;
  const needsBinary = profile.mode === "aft";
  if (needsBinary) {
    if (!binaryPath) binaryPath = findBinary();
    console.log(`  AFT binary: ${binaryPath}`);
    if (profile.requiresFeature) {
      console.warn(`  NOTE: profile requires AFT feature "${profile.requiresFeature}". This script cannot verify feature flags in a prebuilt binary.`);
    }
  } else if (profile.cliKind) {
    const cliPath = findCliBinary(profile.cliKind);
    if (!cliPath) {
      console.warn(`  WARNING: "${profile.cliKind}" not found on PATH. CLI-based search will return empty results.`);
    } else {
      console.log(`  CLI tool: ${cliPath}`);
    }
  }

  const fixture: Fixture = JSON.parse(readFileSync(resolve("benchmarks/semble/fixtures.json"), "utf-8"));
  const allAnnotations: Array<Annotation & { repo_name: string }> = [];

  for (const repo of fixture.repos) {
    const annPath = resolve(`benchmarks/semble/annotations/${repo.name}.json`);
    if (!existsSync(annPath)) continue;
    const anns: Annotation[] = JSON.parse(readFileSync(annPath, "utf-8"));
    for (const ann of anns) allAnnotations.push({ ...ann, repo_name: repo.name });
  }

  console.log(`\nRunning Semble benchmark: ${allAnnotations.length} queries across ${fixture.repos.length} repos (k=${opts.k}, profile=${profile.id}${opts.rerank ? `, rerank candidates=${opts.rerankCandidates}` : ""})`);

  const allResults: BenchResult[] = [];

  for (const repo of fixture.repos) {
    const repoDir = join(resolve(opts.cacheDir), repo.name);
    if (!existsSync(repoDir)) {
      console.log(`  Skipping ${repo.name} — not cloned at ${repoDir}`);
      continue;
    }

    const searchRoot = repoSearchRoot(repoDir, repo);
    if (!existsSync(searchRoot)) {
      console.log(`  Skipping ${repo.name} — benchmark root not found at ${searchRoot}`);
      continue;
    }

    const repoAnnotations = allAnnotations.filter((ann) => ann.repo_name === repo.name);
    if (repoAnnotations.length === 0) continue;

    console.log(`\n  ${repo.name}: ${repoAnnotations.length} queries, root=${repoRootLabel(repoDir, repo)}`);
    const beforeCount = allResults.length;

    try {
      if (profile.mode === "aft") {
        if (!profile.getAftSemanticConfig) throw new Error(`AFT profile ${profile.id} has no semantic config factory`);

        if (opts.rerank && profile.supportsRerank && health.rerankAvailable) {
          const rows = await runAftPass({
            binaryPath,
            repo,
            repoDir: searchRoot,
            annotations: repoAnnotations,
            semanticConfig: profile.getAftSemanticConfig(true, opts.rerankCandidates),
            passLabel: "rerank",
            modeLabel: `${profile.label}+rerank`,
            k: opts.k,
          });
          allResults.push(...rows);
        } else {
          const rows = await runAftPass({
            binaryPath,
            repo,
            repoDir: searchRoot,
            annotations: repoAnnotations,
            semanticConfig: profile.getAftSemanticConfig(false, 0),
            passLabel: "single-pass",
            modeLabel: profile.label,
            k: opts.k,
          });
          allResults.push(...rows);
        }
      } else {
        if (!profile.cliKind) throw new Error(`CLI profile ${profile.id} has no cliKind`);
        const fetchK = opts.rerank && profile.supportsExternalRerank ? opts.rerankCandidates : opts.k;

        for (const ann of repoAnnotations) {
          const relevant = buildRelevantPaths(ann);
          const start = performance.now();
          const raw = profile.cliKind === "semble"
            ? sembleSearch(ann.query, searchRoot, null, fetchK)
            : colgrepSearch(ann.query, searchRoot, null, fetchK);

          let results = raw.results;
          let latency = raw.latency_ms;
          if (opts.rerank && profile.supportsExternalRerank && health.rerankAvailable) {
            results = await rerankResults(ann.query, results, opts.k, opts.rerankCandidates, searchRoot);
            latency = performance.now() - start;
          }

          allResults.push(makeBenchResult(profile.label, ann, repo.name, results, relevant, latency, opts.k));
        }
      }
    } catch (err) {
      console.error(`    ${repo.name}: ERROR — ${err}`);
      if (opts.failFast) throw err;
    }

    const repoResults = allResults.slice(beforeCount);
    if (repoResults.length > 0) printRepoRows(repo.name, profile.label, repoResults);
  }

  const aggregate = aggregateByMode(allResults);
  const byCategory = aggregateNested(allResults, "category");
  const byRepo = aggregateNested(allResults, "repo_name");

  const report: BenchReport = {
    timestamp: new Date().toISOString(),
    profile: profile.id,
    profile_label: profile.label,
    k: opts.k,
    binary: needsBinary ? binaryPath : profile.cliKind || "",
    openai_stack: {
      embedding: {
        model: OPENAI_STACK.embedding.model,
        base_url: OPENAI_STACK.embedding.baseUrl,
        port: OPENAI_STACK.embedding.port,
      },
      reranker: {
        model: OPENAI_STACK.reranker.model,
        base_url: OPENAI_STACK.reranker.baseUrl,
        port: OPENAI_STACK.reranker.port,
        max_candidates: opts.rerankCandidates,
      },
    },
    results: allResults,
    aggregate,
    by_category: byCategory,
    by_repo: byRepo,
  };

  writeFileSync(resolve(opts.outputFile), `${JSON.stringify(report, null, 2)}\n`);

  console.log("\n── Per-repo results are printed inline above ──");
  printSummaryTable(aggregate, opts.k, profile.id);

  console.log("\nBy category:");
  for (const [cat, modes] of Object.entries(byCategory)) {
    for (const [mode, data] of Object.entries(modes)) {
      console.log(`  ${cat}/${mode}: recall=${(data.recall * 100).toFixed(1)}% mrr=${data.mrr.toFixed(3)} count=${data.count}`);
    }
  }

  console.log(`\nReport saved to ${opts.outputFile}`);
}

main().catch((err) => {
  console.error("Fatal:", err instanceof Error ? err.message : err);
  process.exit(1);
});
