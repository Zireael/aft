#!/usr/bin/env bun
/**
 * Semble-inspired benchmark runner for AFT semantic search.
 *
 * Spawns the AFT binary per repo, sends configure + semantic_eval commands
 * over NDJSON, and computes recall@k, MRR across all 50 queries.
 *
 * Usage:
 *   bun run benchmarks/semble/run-semble-bench.ts [options]
 *
 * Options:
 *   --k <n>              Top-k for recall (default: 10)
 *   --cache-dir <dir>    Repo cache directory (default: .bench-cache)
 *   --output <file>      Output report (default: semble-bench-report.json)
 *   --binary <path>      AFT binary path (default: auto-detect)
 */

import { readFileSync, writeFileSync, existsSync } from "fs";
import { join, resolve } from "path";
import { spawn, type ChildProcess } from "child_process";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface Annotation {
  query: string;
  /** Relevant file paths (string) or objects with {path, start_line, end_line}. */
  relevant: (string | { path: string; start_line?: number; end_line?: number })[];
  /** Secondary file paths (string) or objects with {path, start_line, end_line}. */
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
  profile?: string;
  profile_label?: string;
  k: number;
  binary: string;
  embedding_model?: string;
  reranker_model?: string;
  results: BenchResult[];
  aggregate: Record<
    string,
    {
      recall: number;
      mrr: number;
      count: number;
      mean_latency_ms: number;
    }
  >;
  by_category: Record<
    string,
    Record<string, { recall: number; mrr: number; count: number }>
  >;
  by_repo: Record<
    string,
    Record<string, { recall: number; mrr: number; count: number }>
  >;
}

// ---------------------------------------------------------------------------
// Profile definitions
// ---------------------------------------------------------------------------

type ProfileMode = "aft" | "cli";

interface Profile {
  id: string;
  label: string;
  description: string;
  mode: ProfileMode;
  /** Configure payload sent to AFT binary (profiles a-d) */
  configurePayload?: Record<string, unknown>;
  /** CLI command for spawning search (profiles e-g) */
  cliCommand?: string;
  /** CLI argument template — {query}, {path}, {k} are substituted */
  cliArgsTemplate?: string;
  /** Whether CLI output is JSON (parsed) or text (grepped) */
  cliOutputJson?: boolean;
  /** Endpoint to ping before benchmarking */
  pingEndpoint?: { url: string; model: string; type: "embedding" | "reranker" };
  /** Reranker configuration (profiles d, g) */
  rerankerConfig?: { baseUrl: string; model: string };
  /** Cargo feature required by this profile (checked against binary) */
  requiresFeature?: string;
  /**
   * Dual-mode: run both pre-rerank (embedding-only) and post-rerank (embedding+reranker)
   * passes for each repo. Results stored with mode labels "aft" (pre) and "aft+rerank" (post).
   * Only valid for mode="aft" profiles.
   */
  dual?: boolean;
}

const PROFILES: Record<string, Profile> = {
  a: {
    id: "a",
    label: "fastembed",
    description: "fastembed (built-in ONNX) — all-MiniLM-L6-v2",
    mode: "aft",
    configurePayload: {
      backend: "fastembed",
      model: "all-MiniLM-L6-v2",
      diagnostics_enabled: true,
    },
  },
  b: {
    id: "b",
    label: "model2vec",
    description: "model2vec — Potion Code 16M (local CPU, 256 dims) [requires --features semantic-model2vec]",
    mode: "aft",
    configurePayload: {
      backend: "model2vec",
      model: "minishlab/potion-code-16M",
      model_path: "D:/AI/LLM_models/potion-code-16M",
      diagnostics_enabled: true,
    },
    // Model2Vec requires the `semantic-model2vec` Cargo feature.
    // The release binary does NOT include this feature by default.
    // Build with: cargo build --release --features semantic-model2vec
    requiresFeature: "semantic-model2vec",
  },
  c: {
    id: "c",
    label: "oasis",
    description: "OpenAI-compatible → OASIS at 127.0.0.1:10002",
    mode: "aft",
    configurePayload: {
      backend: "openai_compatible",
      base_url: "http://127.0.0.1:10002/v1",
      model: "OASIS-code-embedding-1.5B.i1-Q4_K_M",
      diagnostics_enabled: true,
      max_batch_size: 16,
      timeout_ms: 60_000,
    },
    pingEndpoint: {
      url: "http://127.0.0.1:10002/v1/embeddings",
      model: "OASIS-code-embedding-1.5B.i1-Q4_K_M",
      type: "embedding",
    },
  },
  d: {
    id: "d",
    label: "oasis+rerank",
    description: "OASIS + reranker at 127.0.0.1:10001 (CodeRankLLM)",
    mode: "aft",
    configurePayload: {
      backend: "openai_compatible",
      base_url: "http://127.0.0.1:10002/v1",
      model: "OASIS-code-embedding-1.5B.i1-Q4_K_M",
      diagnostics_enabled: true,
      max_batch_size: 16,
      timeout_ms: 60_000,
      rerank_enabled: true,
      rerank_model: "CodeRankLLM.Q4_K_M",
      rerank_base_url: "http://127.0.0.1:10001/v1",
      rerank_timeout_ms: 30_000,
      rerank_max_candidates: 30,
    },
    pingEndpoint: {
      url: "http://127.0.0.1:10002/v1/embeddings",
      model: "OASIS-code-embedding-1.5B.i1-Q4_K_M",
      type: "embedding",
    },
    rerankerConfig: {
      baseUrl: "http://127.0.0.1:10001/v1",
      model: "CodeRankLLM.Q4_K_M",
    },
    /** Dual-mode: reports both pre-rerank and post-rerank metrics for comparison */
    dual: true,
  },
  e: {
    id: "e",
    label: "semble",
    description: "Semble CLI — `semble search` with semantic embeddings",
    mode: "cli",
    cliCommand: "semble",
    cliArgsTemplate: 'search --top-k {k} --content all "{query}" "{path}"',
    cliOutputJson: false,
  },
  f: {
    id: "f",
    label: "colgrep",
    description: "colgrep CLI — semantic code search CLI",
    mode: "cli",
    cliCommand: "colgrep",
    cliArgsTemplate: '"{query}" --results {k} "{path}"',
    cliOutputJson: false,
  },
  g: {
    id: "g",
    label: "semble+rerank",
    description: "Semble CLI + reranker at 127.0.0.1:10002",
    mode: "cli",
    cliCommand: "semble",
    cliArgsTemplate: 'search --top-k {k} --content all "{query}" "{path}"',
    cliOutputJson: false,
    rerankerConfig: {
      baseUrl: "http://127.0.0.1:10002/v1",
      model: "CodeRankLLM.Q4_K_M",
    },
  },
};

// ---------------------------------------------------------------------------
// Scoring helpers
// ---------------------------------------------------------------------------

function normalizePath(p: string): string {
  if (typeof p !== "string") return String(p ?? "");
  return p.replace(/\\/g, "/").replace(/^\.\//, "");
}

/** Normalize a relevant annotation entry (string or {path: string}) to a file path string. */
function normalizeAnnotationPath(r: string | { path: string; [key: string]: unknown }): string {
  return typeof r === "string" ? r : r.path;
}

/** Build a flat list of normalized file paths from an annotation's relevant + secondary arrays. */
function buildRelevantPaths(ann: Annotation): string[] {
  const all = [...(ann.relevant || []), ...(ann.secondary || [])];
  return all.map((entry) => normalizeAnnotationPath(entry));
}

function recallAtK(
  retrieved: Array<{ file: string }>,
  relevant: string[],
  k: number
): number {
  if (relevant.length === 0) return 0;
  const rPaths = new Set(retrieved.slice(0, k).map((r) => normalizePath(r.file)));
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

function mrr(
  retrieved: Array<{ file: string }>,
  relevant: string[]
): number {
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
// AFT binary interaction via NDJSON
// ---------------------------------------------------------------------------

function findBinary(): string {
  const localAppData = process.env.LOCALAPPDATA || "";
  const home = process.env.HOME || process.env.USERPROFILE || "";
  const candidates = [
    resolve("crates/aft/target/release/aft.exe"),
    resolve("crates/aft/target/release/aft"),
    // Windows: %LOCALAPPDATA%/aft/bin/v*/aft.exe
    ...(localAppData ? [join(localAppData, "aft/bin/aft.exe")] : []),
    // macOS/Linux: ~/.cache/aft/bin/aft
    join(home, ".cache/aft/bin/aft"),
  ];
  // Also scan for versioned Windows paths
  if (localAppData) {
    try {
      const { readdirSync } = require("fs");
      const binDir = join(localAppData, "aft/bin");
      if (existsSync(binDir)) {
        for (const v of readdirSync(binDir)) {
          const exe = join(binDir, v, "aft.exe");
          if (existsSync(exe)) candidates.push(exe);
        }
      }
    } catch {}
  }
  for (const c of candidates) {
    if (existsSync(c)) return c;
  }
  throw new Error("AFT binary not found. Build with cargo or set --binary.");
}

interface BridgeResponse {
  id: string;
  success: boolean;
  data?: Record<string, unknown>;
  text?: string;
  message?: string;
  code?: string;
}

class AftBridge {
  private proc: ChildProcess;
  private pending = new Map<string, {
    resolve: (v: BridgeResponse) => void;
    reject: (e: Error) => void;
  }>();
  private buffer = "";
  private configured = false;
  private stderrLines: string[] = [];
  private label: string;

  constructor(binaryPath: string, projectRoot: string, label = "") {
    this.label = label || projectRoot.split(/[\\/]/).pop() || "unknown";

    this.proc = spawn(binaryPath, [], {
      stdio: ["pipe", "pipe", "pipe"],
      env: { ...process.env, RUST_LOG: "info" },
    });

    this.proc.stdout!.on("data", (chunk: Buffer) => {
      this.buffer += chunk.toString();
      const lines = this.buffer.split("\n");
      this.buffer = lines.pop() || "";
      for (const line of lines) {
        const trimmed = line.trim();
        if (!trimmed) continue;
        // Skip push frames (status_bar, bash_completed, etc.) — not responses
        if (trimmed.includes('"type"') && !trimmed.includes('"id"')) continue;
        try {
          const msg = JSON.parse(trimmed) as BridgeResponse;
          const p = this.pending.get(msg.id);
          if (p) {
            this.pending.delete(msg.id);
            p.resolve(msg);
          }
        } catch {}
      }
    });

    // Pipe stderr lines through with a tag — critical for debugging hangs
    this.proc.stderr!.on("data", (chunk: Buffer) => {
      for (const line of chunk.toString().split("\n")) {
        if (line.trim()) {
          this.stderrLines.push(line.trim());
          // Keep last 100 lines only
          if (this.stderrLines.length > 100) this.stderrLines.shift();
        }
      }
    });
  }

  async send(command: string, params: Record<string, unknown>): Promise<BridgeResponse> {
    const id = `${command}-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
    const request = {
      id,
      command,
      ...params,
    };
    return new Promise<BridgeResponse>((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.proc.stdin!.write(JSON.stringify(request) + "\n");
      // Timeout after 60s
      setTimeout(() => {
        if (this.pending.has(id)) {
          this.pending.delete(id);
          reject(new Error(`Timeout on ${command}`));
        }
      }, 60_000);
    });
  }

  /** Send configure and wait for it to complete. Returns the raw response. */
  async sendConfigure(projectRoot: string): Promise<BridgeResponse> {
    return this.sendConfigureEx(projectRoot, {
      backend: "openai_compatible",
      base_url: "http://127.0.0.1:10002/v1",
      model: "OASIS-code-embedding-1.5B.i1-Q4_K_M",
      diagnostics_enabled: true,
    });
  }

  /** Send configure with a profile-specific semantic payload. */
  async sendConfigureEx(projectRoot: string, semanticPayload: Record<string, unknown>): Promise<BridgeResponse> {
    const resp = await this.send("configure", {
      project_root: projectRoot,
      harness: "opencode",
      semantic_search: true,
      search_index: true,
      semantic: semanticPayload,
    });
    if (!resp.success) {
      const tail = this.getStderrTail(10);
      throw new Error(
        `Configure failed for ${this.label}: ${resp.message ?? resp.code ?? "unknown"}\n` +
        (tail.length ? `Stderr tail:\n${tail.join("\n")}` : "(no stderr)")
      );
    }
    this.configured = true;
    return resp;
  }

  getStderrTail(n = 20): string[] {
    return this.stderrLines.slice(-n);
  }

  async waitReady(): Promise<void> {
    // Poll by sending probe searches. The binary uses #[serde(flatten)] so
    // all response fields (status, results, text) are at the TOP LEVEL —
    // there is no "data" wrapper. "building" responses have status:"building"
    // with no "results" field; ready responses have results:[...] array.
    // Poll up to 300 times (10 minutes with 2s intervals) — pydantic repos
    // can take several minutes to build the chunk index on first run.
    for (let i = 0; i < 300; i++) {
      try {
        const resp = await this.send("semantic_search", { query: "function", top_k: 3 });
        const status = (resp as any).status;
        if (status === "building") {
          if (i % 5 === 0) {
            const stage = (resp as any).stage || "?";
            console.log(`      [poll ${i}] building (${stage})`);
          }
        } else if (resp.success && Array.isArray((resp as any).results) && (resp as any).results.length > 0) {
          return; // Index has content — ready
        } else if (!resp.success) {
          const msg = resp.message ?? resp.code;
          if (i % 5 === 0) console.log(`      [poll ${i}] probe: ${msg}`);
        } else {
          // success but no results — could be "ready" with empty results
          // or some other status. Check text for building indicator.
          const text = String(resp.text || "");
          if (text.includes("building")) {
            if (i % 5 === 0) console.log(`      [poll ${i}] building (via text)`);
          } else {
            return; // No building indicator — assume ready
          }
        }
      } catch (err) {
        if (i % 5 === 0) {
          console.log(`      [poll ${i}] error: ${err}`);
          const tail = this.getStderrTail(5);
          if (tail.length) console.log(`      stderr: ${tail.join("\n      stderr: ")}`);
        }
      }
      await new Promise((r) => setTimeout(r, 2000));
    }
    const tail = this.getStderrTail(30);
    throw new Error(
      `Timeout waiting for AFT to be ready (${this.label})\n` +
      (tail.length ? `Last stderr:\n${tail.join("\n")}` : "(no stderr captured)")
    );
  }

  async search(
    query: string,
    topK: number
  ): Promise<{ results: Array<{ file: string; score?: number }>; latency_ms: number }> {
    const start = performance.now();
    const resp = await this.send("semantic_search", {
      query,
      top_k: topK,
    });
    const latency_ms = performance.now() - start;

    // The binary uses #[serde(flatten)] on Response.data, so all fields
    // (results, status, text) are at the TOP LEVEL of the JSON object.
    const raw = resp as any;
    const resultsArr = raw.results;

    if (!resp.success) {
      if (this._searchFailCount === undefined) this._searchFailCount = 0;
      if (this._searchFailCount < 3) {
        console.log(`      [search] FAIL (${latency_ms.toFixed(0)}ms): ${resp.message ?? resp.code ?? resp.text ?? "unknown"}`);
        this._searchFailCount++;
      }
      return { results: [], latency_ms };
    }

    if (!Array.isArray(resultsArr)) {
      if (this._searchFailCount === undefined) this._searchFailCount = 0;
      if (this._searchFailCount < 3) {
        console.log(`      [search] NO RESULTS ARRAY (${latency_ms.toFixed(0)}ms): status=${raw.status} keys=[${Object.keys(raw).join(",")}]`);
        if (raw.text) console.log(`      [search] text: ${String(raw.text).slice(0, 200)}`);
        this._searchFailCount++;
      }
      return { results: [], latency_ms };
    }

    return {
      results: resultsArr.map((r: Record<string, unknown>) => ({
        file: String(r.file || ""),
        score: typeof r.score === "number" ? r.score : undefined,
      })),
      latency_ms,
    };
  }

  private _searchFailCount?: number;

  shutdown(): void {
    this.proc.kill();
  }
}

// ---------------------------------------------------------------------------
// CLI-based search (profiles e, f, g)
// ---------------------------------------------------------------------------

import { execSync } from "child_process";

interface CliSearchResult {
  file: string;
  score?: number;
  line?: number;
}

/** Run semble CLI search against a repo directory. */
function sembleSearch(
  query: string,
  searchDir: string,
  benchmarkRoot: string | null,
  k: number
): { results: CliSearchResult[]; latency_ms: number } {
  const targetDir = benchmarkRoot ? join(searchDir, benchmarkRoot) : searchDir;
  const start = performance.now();
  let results: CliSearchResult[] = [];
  try {
    const output = execSync(
      `semble search --top-k ${k} --content all "${query.replace(/"/g, '\\"')}" "${targetDir}"`,
      { encoding: "utf-8", stdio: "pipe", timeout: 30_000 }
    ).toString().trim();
    if (output) {
      // Try JSON first
      try {
        const parsed = JSON.parse(output);
        if (Array.isArray(parsed)) {
          results = parsed.slice(0, k).map((r: Record<string, unknown>) => ({
            file: String(r.file_path ?? r.file ?? ""),
            score: typeof r.score === "number" ? r.score : undefined,
            line: typeof r.line === "number" ? r.line : undefined,
          }));
        }
      } catch {
        // Not JSON — try line-by-line parse (tab-separated or path-like)
        const lines = output.split("\n").filter(Boolean);
        for (const line of lines.slice(0, k)) {
          const parts = line.split("\t");
          const file = parts[0]?.trim() || line.trim();
          if (file) results.push({ file });
        }
      }
    }
  } catch (err: any) {
    // execSync throws on non-zero exit — semble returns non-zero for no results
    if ((err as any)?.stderr) {
      const stderr = (err as any).stderr.toString();
      if (stderr.includes("not found") || stderr.includes("No such file")) {
        // CLI not available — return empty results
      }
    }
  }
  return { results, latency_ms: performance.now() - start };
}

/** Run colgrep CLI search against a repo directory. */
function colgrepSearch(
  query: string,
  searchDir: string,
  benchmarkRoot: string | null,
  k: number
): { results: CliSearchResult[]; latency_ms: number } {
  const targetDir = benchmarkRoot ? join(searchDir, benchmarkRoot) : searchDir;
  const start = performance.now();
  let results: CliSearchResult[] = [];
  try {
    const output = execSync(
      `colgrep "${query.replace(/"/g, '\\"')}" --results ${k} "${targetDir}"`,
      { encoding: "utf-8", stdio: "pipe", timeout: 30_000 }
    ).toString().trim();
    if (output) {
      // Try JSON first
      try {
        const parsed = JSON.parse(output);
        if (Array.isArray(parsed)) {
          results = parsed.slice(0, k).map((r: Record<string, unknown>) => ({
            file: String(r.file_path ?? r.file ?? ""),
            score: typeof r.score === "number" ? r.score : undefined,
            line: typeof r.line === "number" ? r.line : undefined,
          }));
        }
      } catch {
        // Not JSON — parse text output
        const lines = output.split("\n").filter(Boolean);
        for (const line of lines.slice(0, k)) {
          const parts = line.split(":");
          const file = parts[0]?.trim();
          if (file) results.push({ file });
        }
      }
    }
  } catch {
    // colgrep not available or no results
  }
  return { results, latency_ms: performance.now() - start };
}

/** Find the colgrep or semble binary on PATH or known locations. */
function findCliBinary(name: string): string | null {
  try {
    const output = execSync(`where ${name} 2>nul || which ${name} 2>/dev/null`, {
      encoding: "utf-8",
      stdio: "pipe",
      timeout: 5_000,
    }).toString().trim();
    if (output) {
      const firstLine = output.split("\n")[0].trim();
      if (firstLine && existsSync(firstLine)) return firstLine;
      return name; // rely on PATH
    }
  } catch {}
  return null;
}

// ---------------------------------------------------------------------------
// Table printing helpers
// ---------------------------------------------------------------------------

/** Print a per-repo summary row. */
function printRepoRow(
  repoName: string,
  profileLabel: string,
  queryCount: number,
  recall: number,
  mrrVal: number,
  meanLatencyMs: number
): void {
  console.log(
    `  ${repoName.padEnd(12)} ${profileLabel.padEnd(16)} ${String(queryCount).padStart(3)} queries` +
    `  recall=${(recall * 100).toFixed(1).padStart(5)}%  mrr=${mrrVal.toFixed(3)}` +
    `  latency=${meanLatencyMs.toFixed(0).padStart(5)}ms`
  );
}

/** Print a final aggregate summary table. */
function printSummaryTable(
  aggregate: Record<string, { recall: number; mrr: number; count: number; mean_latency_ms: number }>,
  k: number,
  profileLabel: string
): void {
  console.log(`\n┌──────────────────────────────────────────────────────────────────────┐`);
  console.log(`│  Semble Benchmark Summary (k=${k}, profile=${profileLabel})`);
  console.log(`├──────────────────────────────────────────────────────────────────────┤`);
  for (const [mode, data] of Object.entries(aggregate)) {
    const label = mode.padEnd(20);
    const recallStr = (data.recall * 100).toFixed(1).padStart(5);
    const mrrStr = data.mrr.toFixed(3);
    const latStr = data.mean_latency_ms.toFixed(0).padStart(5);
    console.log(`│  ${label} recall=${recallStr}%  mrr=${mrrStr}  latency=${latStr}ms  (${data.count} queries)`);
  }
  console.log(`└──────────────────────────────────────────────────────────────────────┘`);
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Model connectivity diagnostics
// ---------------------------------------------------------------------------

const EMBED_URL = "http://127.0.0.1:10002/v1/embeddings";
const EMBED_MODEL = "OASIS-code-embedding-1.5B.i1-Q4_K_M";
const RERANK_URL = "http://127.0.0.1:10002/v1/chat/completions";
const RERANK_MODEL = "CodeRankLLM.Q4_K_M";

async function pingModels(): Promise<{ embeddingOk: boolean; rerankerOk: boolean; embeddingError?: string; rerankerError?: string }> {
  const results = { embeddingOk: false, rerankerOk: false, embeddingError: undefined as string | undefined, rerankerError: undefined as string | undefined };

  // Ping embedding model
  try {
    const resp = await fetch(EMBED_URL, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ input: "test connectivity", model: EMBED_MODEL }),
      signal: AbortSignal.timeout(15_000),
    });
    if (resp.ok) {
      results.embeddingOk = true;
    } else {
      results.embeddingError = `HTTP ${resp.status}: ${await resp.text().catch(() => "no body")}`;
    }
  } catch (err) {
    results.embeddingError = String(err);
  }

  // Ping reranking model
  try {
    const resp = await fetch(RERANK_URL, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        model: RERANK_MODEL,
        messages: [{ role: "user", content: "Rank: A B C" }],
        temperature: 0.0,
        max_tokens: 50,
      }),
      signal: AbortSignal.timeout(15_000),
    });
    if (resp.ok) {
      results.rerankerOk = true;
    } else {
      results.rerankerError = `HTTP ${resp.status}: ${await resp.text().catch(() => "no body")}`;
    }
  } catch (err) {
    results.rerankerError = String(err);
  }

  return results;
}

async function main() {
  const args = process.argv.slice(2);
  let k = 10;
  let cacheDir = ".bench-cache";
  let outputFile = "semble-bench-report.json";
  let binaryPath = "";
  let profileId = "c";

  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--k":
        k = parseInt(args[++i], 10);
        break;
      case "--cache-dir":
        cacheDir = args[++i];
        break;
      case "--output":
        outputFile = args[++i];
        break;
      case "--binary":
        binaryPath = args[++i];
        break;
      case "--profile":
        profileId = args[++i];
        break;
    }
  }

  // Resolve profile
  const profile = PROFILES[profileId];
  if (!profile) {
    console.error(`Unknown profile "${profileId}". Available profiles:`);
    for (const [id, p] of Object.entries(PROFILES)) {
      console.error(`  ${id}: ${p.description}`);
    }
    process.exit(1);
  }
  console.log(`Using profile ${profileId}: ${profile.description}`);

  // Binary resolution (only needed for AFT profiles)
  const needsBinary = profile.mode === "aft";
  if (needsBinary) {
    if (!binaryPath) binaryPath = findBinary();
    console.log(`  AFT binary: ${binaryPath}`);
  } else {
    // Verify CLI tool availability
    const cliName = profile.cliCommand || "";
    const cliPath = findCliBinary(cliName);
    if (!cliPath) {
      console.warn(`  WARNING: "${cliName}" not found on PATH. CLI-based search may fail.`);
    } else {
      console.log(`  CLI tool: ${cliPath}`);
    }
  }

  // Model connectivity check (profile-specific endpoints)
  console.log("\n--- Model connectivity check ---");
  if (profile.pingEndpoint) {
    const ep = profile.pingEndpoint;
    try {
      const resp = await fetch(ep.url, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ input: "test connectivity", model: ep.model }),
        signal: AbortSignal.timeout(15_000),
      });
      if (resp.ok) {
        console.log(`  ${ep.type === "embedding" ? "Embedding" : "Reranker"} (${ep.model}): ✓ reachable`);
      } else {
        console.warn(`  ${ep.type === "embedding" ? "Embedding" : "Reranker"} (${ep.model}): ✗ HTTP ${resp.status}`);
      }
    } catch (err) {
      console.warn(`  ${ep.type === "embedding" ? "Embedding" : "Reranker"} (${ep.model}): ✗ ${err}`);
    }
  } else {
    // Try generic ping for OASIS endpoint anyway
    const genStatus = await pingModels();
    console.log(`  Embedding (${EMBED_MODEL}): ${genStatus.embeddingOk ? "✓ reachable" : `✗ ${genStatus.embeddingError}`}`);
    console.log(`  Reranker  (${RERANK_MODEL}): ${genStatus.rerankerOk ? "✓ reachable" : `✗ ${genStatus.rerankerError}`}`);
  }

  if (profile.rerankerConfig) {
    const rc = profile.rerankerConfig;
    try {
      const resp = await fetch(`${rc.baseUrl}/chat/completions`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          model: rc.model,
          messages: [{ role: "user", content: "Rank: A B C" }],
          temperature: 0.0,
          max_tokens: 50,
        }),
        signal: AbortSignal.timeout(15_000),
      });
      if (resp.ok) {
        console.log(`  Reranker (${rc.model} @ ${rc.baseUrl}): ✓ reachable`);
      } else {
        console.warn(`  Reranker (${rc.model} @ ${rc.baseUrl}): ✗ HTTP ${resp.status} — will run without reranking`);
      }
    } catch (err) {
      console.warn(`  Reranker (${rc.model} @ ${rc.baseUrl}): ✗ ${err} — will run without reranking`);
    }
  }
  console.log("");

  // Load fixtures
  const fixture: Fixture = JSON.parse(
    readFileSync(resolve("benchmarks/semble/fixtures.json"), "utf-8")
  );

  // Load all annotations
  const allAnnotations: Array<Annotation & { repo_name: string }> = [];
  for (const repo of fixture.repos) {
    const annPath = resolve(`benchmarks/semble/annotations/${repo.name}.json`);
    if (!existsSync(annPath)) continue;
    const anns: Annotation[] = JSON.parse(readFileSync(annPath, "utf-8"));
    for (const a of anns) {
      allAnnotations.push({ ...a, repo_name: repo.name });
    }
  }

  const modeLabel = profile.mode === "aft" ? "aft" : profile.label;
  console.log(
    `Running Semble benchmark: ${allAnnotations.length} queries across ${fixture.repos.length} repos (k=${k}, profile=${profileId})`
  );

  const allResults: BenchResult[] = [];
  const repoSummaries: Array<{
    repo: string;
    recall: number;
    mrr: number;
    latency_ms: number;
    count: number;
  }> = [];

  for (const repo of fixture.repos) {
    const repoDir = join(resolve(cacheDir), repo.name);
    if (!existsSync(repoDir)) {
      console.log(`  Skipping ${repo.name} — not cloned at ${repoDir}`);
      continue;
    }

    const repoAnnotations = allAnnotations.filter(
      (a) => a.repo_name === repo.name
    );
    if (repoAnnotations.length === 0) continue;

    console.log(`\n  ${repo.name}: ${repoAnnotations.length} queries, root=${repo.benchmark_root || "."}`);

    if (profile.mode === "aft") {
      // ── AFT binary mode (profiles a-d) ──

      /** Run a complete pass: configure → wait → search all queries → collect results */
      const runPass = async (
        passLabel: string,
        passModeLabel: string,
        configOverrides: Record<string, unknown>,
      ): Promise<void> => {
        const bridge = new AftBridge(binaryPath, repoDir, `${repo.name}/${passLabel}`);
        try {
          console.log(`    ${repo.name} (${passLabel}): configuring...`);
          await bridge.sendConfigureEx(repoDir, {
            ...profile.configurePayload,
            ...configOverrides,
          });
          console.log(`    ${repo.name} (${passLabel}): configured ✓`);

          await bridge.waitReady();
          console.log(`    ${repo.name} (${passLabel}): index ready ✓`);

          for (const ann of repoAnnotations) {
            const allRelevant = buildRelevantPaths(ann);
            const { results, latency_ms } = await bridge.search(ann.query, k);

            allResults.push({
              mode: passModeLabel,
              query: ann.query,
              repo_name: repo.name,
              category: ann.category,
              latency_ms,
              results,
              recall_at_k: recallAtK(results, allRelevant, k),
              mrr: mrr(results, allRelevant),
            });
          }
        } catch (err) {
          console.error(`    ${repo.name} (${passLabel}): ERROR — ${err}`);
        } finally {
          bridge.shutdown();
        }
      };

      if (profile.dual) {
        // Dual-mode: run pre-rerank (embedding-only) then post-rerank (embedding+reranker)
        // Pre-rerank pass: disable reranker, remove reranker-specific fields
        const preRerankConfig: Record<string, unknown> = {
          rerank_enabled: false,
        };
        await runPass("pre-rerank", "aft", preRerankConfig);

        // Post-rerank pass: use profile defaults (rerank_enabled: true + reranker config)
        await runPass("post-rerank", "aft+rerank", {});
      } else {
        // Single pass: use profile payload as-is
        await runPass("single-pass", modeLabel, {});
      }
    } else {
      // ── CLI mode (profiles e-g) ──
      const searchFn = profile.cliCommand === "semble" ? sembleSearch
        : profile.cliCommand === "colgrep" ? colgrepSearch
        : null;

      if (!searchFn) {
        console.error(`    ${repo.name}: unknown CLI command "${profile.cliCommand}"`);
        continue;
      }

      for (const ann of repoAnnotations) {
        const allRelevant = buildRelevantPaths(ann);

        const { results, latency_ms } = searchFn(
          ann.query,
          repoDir,
          repo.benchmark_root,
          k
        );

        allResults.push({
          mode: modeLabel,
          query: ann.query,
          repo_name: repo.name,
          category: ann.category,
          latency_ms,
          results,
          recall_at_k: recallAtK(results, allRelevant, k),
          mrr: mrr(results, allRelevant),
        });
      }
    }

    // Per-repo summary after processing
    const repoResults = allResults.filter((r) => r.repo_name === repo.name);
    if (repoResults.length > 0) {
      const avgRecall = repoResults.reduce((s, r) => s + r.recall_at_k, 0) / repoResults.length;
      const avgMrr = repoResults.reduce((s, r) => s + r.mrr, 0) / repoResults.length;
      const avgLat = repoResults.reduce((s, r) => s + r.latency_ms, 0) / repoResults.length;
      repoSummaries.push({
        repo: repo.name,
        recall: avgRecall,
        mrr: avgMrr,
        latency_ms: avgLat,
        count: repoResults.length,
      });
      printRepoRow(repo.name, profile.label, repoResults.length, avgRecall, avgMrr, avgLat);
    }
  }

  // Aggregate
  const aggregate: Record<
    string,
    { recalls: number[]; mrrs: number[]; lats: number[] }
  > = {};
  for (const r of allResults) {
    if (!aggregate[r.mode])
      aggregate[r.mode] = { recalls: [], mrrs: [], lats: [] };
    aggregate[r.mode].recalls.push(r.recall_at_k);
    aggregate[r.mode].mrrs.push(r.mrr);
    aggregate[r.mode].lats.push(r.latency_ms);
  }

  const aggregateOut: Record<string, { recall: number; mrr: number; count: number; mean_latency_ms: number }> = {};
  for (const [mode, data] of Object.entries(aggregate)) {
    const n = data.recalls.length;
    aggregateOut[mode] = {
      recall: data.recalls.reduce((s, v) => s + v, 0) / n,
      mrr: data.mrrs.reduce((s, v) => s + v, 0) / n,
      count: n,
      mean_latency_ms: data.lats.reduce((s, v) => s + v, 0) / n,
    };
  }

  // By category
  const byCategory: Record<string, Record<string, { recalls: number[]; mrrs: number[] }>> = {};
  for (const r of allResults) {
    if (!byCategory[r.category]) byCategory[r.category] = {};
    if (!byCategory[r.category][r.mode])
      byCategory[r.category][r.mode] = { recalls: [], mrrs: [] };
    byCategory[r.category][r.mode].recalls.push(r.recall_at_k);
    byCategory[r.category][r.mode].mrrs.push(r.mrr);
  }
  const byCategoryOut: Record<string, Record<string, { recall: number; mrr: number; count: number }>> = {};
  for (const [cat, modes] of Object.entries(byCategory)) {
    byCategoryOut[cat] = {};
    for (const [mode, data] of Object.entries(modes)) {
      const n = data.recalls.length;
      byCategoryOut[cat][mode] = {
        recall: data.recalls.reduce((s, v) => s + v, 0) / n,
        mrr: data.mrrs.reduce((s, v) => s + v, 0) / n,
        count: n,
      };
    }
  }

  // By repo
  const byRepo: Record<string, Record<string, { recalls: number[]; mrrs: number[] }>> = {};
  for (const r of allResults) {
    if (!byRepo[r.repo_name]) byRepo[r.repo_name] = {};
    if (!byRepo[r.repo_name][r.mode])
      byRepo[r.repo_name][r.mode] = { recalls: [], mrrs: [] };
    byRepo[r.repo_name][r.mode].recalls.push(r.recall_at_k);
    byRepo[r.repo_name][r.mode].mrrs.push(r.mrr);
  }
  const byRepoOut: Record<string, Record<string, { recall: number; mrr: number; count: number }>> = {};
  for (const [repo, modes] of Object.entries(byRepo)) {
    byRepoOut[repo] = {};
    for (const [mode, data] of Object.entries(modes)) {
      const n = data.recalls.length;
      byRepoOut[repo][mode] = {
        recall: data.recalls.reduce((s, v) => s + v, 0) / n,
        mrr: data.mrrs.reduce((s, v) => s + v, 0) / n,
        count: n,
      };
    }
  }

  const report: BenchReport = {
    timestamp: new Date().toISOString(),
    profile: profileId,
    profile_label: profile.label,
    k,
    binary: needsBinary ? binaryPath : profile.cliCommand || "",
    embedding_model: profile.configurePayload?.model as string | undefined,
    reranker_model: profile.rerankerConfig?.model,
    results: allResults,
    aggregate: aggregateOut,
    by_category: byCategoryOut,
    by_repo: byRepoOut,
  };

  writeFileSync(resolve(outputFile), JSON.stringify(report, null, 2) + "\n");

  // ── Per-repo summary table ──
  console.log(`\n── Per-repo results ──`);
  console.log(`  ${"Repo".padEnd(12)} ${"Profile".padEnd(16)} ${"Queries".padStart(7)}  Recall    MRR       Latency`);
  console.log(`  ${"─".repeat(12)} ${"─".repeat(16)} ${"─".repeat(7)}  ─────── ─────── ─────────`);
  for (const rs of repoSummaries) {
    printRepoRow(rs.repo, profile.label, rs.count, rs.recall, rs.mrr, rs.latency_ms);
  }

  // ── Summary table ──
  printSummaryTable(aggregateOut, k, profileId);

  // ── By category ──
  console.log(`\nBy category:`);
  for (const [cat, modes] of Object.entries(byCategoryOut)) {
    for (const [mode, data] of Object.entries(modes)) {
      console.log(
        `  ${cat}/${mode}: recall=${(data.recall * 100).toFixed(1)}% mrr=${data.mrr.toFixed(3)}`
      );
    }
  }

  console.log(`\nReport saved to ${outputFile}`);
}

main().catch((err) => {
  console.error("Fatal:", err);
  process.exit(1);
});
