/**
 * Benchmark mode adapters for AFT-native and external search modes.
 *
 * Each adapter is a small function with consistent input/output behavior.
 * Mode adapters are registered in one place; no stringly scattered if/else logic.
 */

import { execSync } from "child_process";
import { join } from "path";
import { AftSession } from "./aft-ndjson";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface SearchResult {
  file: string;
  line?: number;
  score?: number;
  content?: string;
  symbol_name?: string;
  symbol_kind?: string;
}

export interface ModeAttempt {
  mode: string;
  status: "ok" | "empty" | "error" | "unavailable" | "timeout";
  reason?: string;
  results: SearchResult[];
  latency_ms: number;
  latency_parts?: Record<string, number>;
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

function normalizePath(p: string): string {
  if (!p) return "";
  return p.replace(/\\/g, "/").replace(/^\/\?\//, "").replace(/^\.\//, "").toLowerCase();
}

// ---------------------------------------------------------------------------
// rg baseline
// ---------------------------------------------------------------------------

export function rgSearch(
  query: string, searchDir: string, benchmarkRoot: string | null, k: number,
): ModeAttempt {
  const targetDir = benchmarkRoot ? join(searchDir, benchmarkRoot) : searchDir;
  const start = performance.now();
  try {
    const output = execSync(
      `rg -n --no-heading --max-count ${k * 2} "${query.replace(/"/g, '\\"')}" .`,
      { cwd: targetDir, encoding: "utf-8", stdio: "pipe", timeout: 10000 },
    );
    const results = output.trim().split("\n").filter(Boolean).slice(0, k).map((line) => {
      const ci = line.indexOf(":");
      const file = line.substring(0, ci);
      const rest = line.substring(ci + 1);
      const ci2 = rest.indexOf(":");
      return { file, line: parseInt(rest.substring(0, ci2), 10) };
    });
    return { mode: "rg", status: results.length > 0 ? "ok" : "empty", results, latency_ms: performance.now() - start };
  } catch {
    return { mode: "rg", status: "empty", results: [], latency_ms: performance.now() - start };
  }
}

// ---------------------------------------------------------------------------
// AFT grep (trigram-indexed)
// ---------------------------------------------------------------------------

export async function aftGrepQuery(
  session: AftSession, query: string, k: number, verbose: boolean,
): Promise<ModeAttempt> {
  const start = performance.now();
  try {
    const resp = await session.call({ command: "grep", pattern: query, max_results: k }, 30_000);
    const items = (resp as any).results || (resp as any).matches;
    const results: SearchResult[] = items && Array.isArray(items)
      ? items.map((r: any) => ({ file: r.file || r.file_path || r.path || "", line: r.start_line || r.line, score: r.score }))
      : [];
    return { mode: "aft-grep", status: results.length > 0 ? "ok" : "empty", results, latency_ms: performance.now() - start };
  } catch (e) {
    if (verbose) console.log(`    GREP ERROR: ${e}`);
    return { mode: "aft-grep", status: "error", reason: String(e), results: [], latency_ms: performance.now() - start };
  }
}

// ---------------------------------------------------------------------------
// FTS5 search
// ---------------------------------------------------------------------------

export async function fts5SearchQuery(
  session: AftSession, query: string, k: number, verbose: boolean,
): Promise<ModeAttempt> {
  const start = performance.now();
  try {
    const resp = await session.call({ command: "fts5_search", query, scope: "all", top_k: k }, 30_000);
    const items = (resp as any).evidence || (resp as any).results || (resp as any).matches;
    const results: SearchResult[] = items && Array.isArray(items)
      ? items.map((r: any) => ({ file: r.file_path || r.path || r.file || "", line: r.start_line || r.line, score: r.score }))
      : [];
    return { mode: "fts5_search", status: results.length > 0 ? "ok" : "empty", results, latency_ms: performance.now() - start };
  } catch (e) {
    if (verbose) console.log(`    FTS5 ERROR: ${e}`);
    return { mode: "fts5_search", status: "error", reason: String(e), results: [], latency_ms: performance.now() - start };
  }
}

// ---------------------------------------------------------------------------
// FTS5 find symbol (exact / prefix)
// ---------------------------------------------------------------------------

export async function fts5FindSymbolQuery(
  session: AftSession, name: string, mode: "exact" | "prefix", k: number, verbose: boolean,
): Promise<ModeAttempt> {
  const modeName = mode === "exact" ? "fts5_find_symbol_exact" : "fts5_find_symbol_prefix";
  const start = performance.now();
  try {
    const resp = await session.call({ command: "fts5_find_symbol", name, mode, top_k: k }, 30_000);
    const items = (resp as any).symbols || (resp as any).results || (resp as any).evidence;
    const results: SearchResult[] = items && Array.isArray(items)
      ? items.map((r: any) => ({
          file: r.file_path || r.path || r.file || "",
          line: r.start_line || r.line,
          score: r.score,
          symbol_name: r.symbol_name || r.name,
          symbol_kind: r.symbol_kind || r.kind,
        }))
      : [];
    return { mode: modeName, status: results.length > 0 ? "ok" : "empty", results, latency_ms: performance.now() - start };
  } catch (e) {
    if (verbose) console.log(`    ${modeName} ERROR: ${e}`);
    return { mode: modeName, status: "error", reason: String(e), results: [], latency_ms: performance.now() - start };
  }
}

// ---------------------------------------------------------------------------
// Glob (path lookup)
// ---------------------------------------------------------------------------

export async function globQuery(
  session: AftSession, pattern: string, verbose: boolean,
): Promise<ModeAttempt> {
  const start = performance.now();
  try {
    const resp = await session.call({ command: "glob", pattern }, 30_000);
    const items = (resp as any).files || (resp as any).results || (resp as any).matches;
    const results: SearchResult[] = items && Array.isArray(items)
      ? items.map((r: any) => ({ file: typeof r === "string" ? r : r.file || r.path || "" }))
      : [];
    return { mode: "glob", status: results.length > 0 ? "ok" : "empty", results, latency_ms: performance.now() - start };
  } catch (e) {
    if (verbose) console.log(`    GLOB ERROR: ${e}`);
    return { mode: "glob", status: "error", reason: String(e), results: [], latency_ms: performance.now() - start };
  }
}

// ---------------------------------------------------------------------------
// AST search (structural)
// ---------------------------------------------------------------------------

export async function astSearchQuery(
  session: AftSession, pattern: string, lang: string, verbose: boolean,
): Promise<ModeAttempt> {
  const start = performance.now();
  try {
    const resp = await session.call({ command: "ast_search", pattern, lang, context: 0 }, 30_000);
    const items = (resp as any).matches || (resp as any).results;
    const results: SearchResult[] = items && Array.isArray(items)
      ? items.map((r: any) => ({
          file: r.file || r.file_path || r.path || "",
          line: r.start_line || r.line || r.range?.start?.line,
          score: r.score,
          content: r.text || r.content,
        }))
      : [];
    return { mode: "ast_search", status: results.length > 0 ? "ok" : "empty", results, latency_ms: performance.now() - start };
  } catch (e) {
    if (verbose) console.log(`    AST ERROR: ${e}`);
    return { mode: "ast_search", status: "error", reason: String(e), results: [], latency_ms: performance.now() - start };
  }
}

// ---------------------------------------------------------------------------
// Semantic search
// ---------------------------------------------------------------------------

export async function semanticQuery(
  session: AftSession, query: string, k: number, backend: string, verbose: boolean,
): Promise<ModeAttempt> {
  const modeName = `semantic_${backend}`;
  const start = performance.now();
  try {
    const resp = await session.call({ command: "semantic_search", query, topK: k }, 30_000);
    const items = (resp as any).results;
    const results: SearchResult[] = items && Array.isArray(items)
      ? items.map((r: any) => ({ file: r.file || r.file_path || r.path || "", line: r.start_line || r.line, score: r.score }))
      : [];
    return { mode: modeName, status: results.length > 0 ? "ok" : "empty", results, latency_ms: performance.now() - start };
  } catch (e) {
    if (verbose) console.log(`    ${modeName} ERROR: ${e}`);
    return { mode: modeName, status: "error", reason: String(e), results: [], latency_ms: performance.now() - start };
  }
}

// ---------------------------------------------------------------------------
// RRF fusion
// ---------------------------------------------------------------------------

export function rrfFusion(a: SearchResult[], b: SearchResult[], k: number): SearchResult[] {
  const K = 60;
  const scoreMap = new Map<string, { result: SearchResult; score: number }>();

  a.forEach((r, i) => {
    const key = normalizePath(r.file);
    const existing = scoreMap.get(key);
    const s = 1 / (K + i + 1);
    if (existing) existing.score += s;
    else scoreMap.set(key, { result: r, score: s });
  });

  b.forEach((r, i) => {
    const key = normalizePath(r.file);
    const existing = scoreMap.get(key);
    const s = 1 / (K + i + 1);
    if (existing) existing.score += s;
    else scoreMap.set(key, { result: r, score: s });
  });

  return [...scoreMap.values()]
    .sort((x, y) => y.score - x.score)
    .slice(0, k)
    .map((v) => v.result);
}

// ---------------------------------------------------------------------------
// Session initializers
// ---------------------------------------------------------------------------

export async function initGrepSession(
  bin: string, targetDir: string, verbose: boolean,
): Promise<AftSession | null> {
  const session = new AftSession(bin);
  try {
    await session.call(
      { command: "configure", harness: "opencode", project_root: targetDir, storage_dir: join(targetDir, ".aft-bench-grep") },
      30_000,
    );
    return session;
  } catch (e) {
    if (verbose) console.log(`    GREP init ERROR: ${e}`);
    session.close();
    return null;
  }
}

export async function initFts5Session(
  bin: string, targetDir: string, verbose: boolean,
): Promise<AftSession | null> {
  const session = new AftSession(bin);
  try {
    await session.call(
      { command: "configure", harness: "opencode", project_root: targetDir, storage_dir: join(targetDir, ".aft-bench-fts5"), fts5: { enabled: true } },
      30_000,
    );
    await session.call({ command: "fts5_index", action: "update" }, 60_000);
    return session;
  } catch (e) {
    if (verbose) console.log(`    FTS5 init ERROR: ${e}`);
    session.close();
    return null;
  }
}

export async function initSemanticSession(
  bin: string, targetDir: string, model: string, backend: string,
  verbose: boolean, storageDir?: string,
): Promise<AftSession | null> {
  const session = new AftSession(bin);
  try {
    const config: Record<string, unknown> = {
      command: "configure", harness: "opencode",
      project_root: targetDir,
      storage_dir: storageDir || join(targetDir, ".aft-bench"),
      semantic_search: true,
    };
    if (backend === "semantic-api") {
      const url = (globalThis as any).__SEMANTIC_API_URL || "";
      const modelName = (globalThis as any).__SEMANTIC_API_MODEL || "";
      config.semantic = { backend: "openai_compatible", base_url: url, model: modelName };
    } else {
      config.semantic = { backend, model };
    }
    await session.call(config, 30_000);

    const deadline = Date.now() + 180_000;
    while (Date.now() < deadline) {
      const status = await session.call({ command: "status" }, 10_000);
      const semStatus = (status as any).semantic_index?.status;
      if (verbose) process.stdout.write(`    SEM-${backend} status: ${semStatus}\r`);
      if (semStatus === "ready" || semStatus === "partial") {
        if (verbose) process.stdout.write(`    SEM-${backend} status: ready     \n`);
        return session;
      }
      if (semStatus === "failed" || semStatus === "disabled") {
        const err = (status as any).semantic_index?.error || "unknown";
        if (verbose) process.stdout.write(`    SEM-${backend} status: ${semStatus} error=${err}     \n`);
        session.close();
        return null;
      }
      await new Promise((r) => setTimeout(r, 1000));
    }
    if (verbose) process.stdout.write(`    SEM-${backend} status: timeout   \n`);
    session.close();
    return null;
  } catch (e) {
    if (verbose) console.log(`    SEM-${backend} init ERROR: ${e}`);
    session.close();
    return null;
  }
}
