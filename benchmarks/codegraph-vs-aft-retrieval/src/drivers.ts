import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { BinaryBridge, ensureOnnxRuntime } from "../../../packages/aft-bridge/src/index.ts";
import type { DriverContext, RankedResult, RetrievalCase, RetrievalDriver } from "./types";
import { HARNESS_DIR, normalizePath, runCommand } from "./util";

type BridgeResponse = Record<string, unknown>;

type PlainObject = Record<string, unknown>;

export function createDriver(name: string, context: DriverContext): RetrievalDriver {
  if (name === "aft") return new AftRetrievalDriver(context);
  if (name === "codegraph") return new CodeGraphRetrievalDriver(context);
  throw new Error(`Unknown retrieval driver: ${name}`);
}

class AftRetrievalDriver implements RetrievalDriver {
  readonly name = "aft" as const;
  private bridge: BinaryBridge | null = null;

  constructor(private readonly context: DriverContext) {}

  async prepare(): Promise<void> {
    if (!existsSync(this.context.aftBinary)) {
      throw new Error(`AFT binary not found: ${this.context.aftBinary}`);
    }
    const storageDir = process.env.AFT_STORAGE_DIR ?? join(homedir(), ".cache", "aft");
    // Semantic embedding backend. `aft warmup` is cloud-blind (always tries
    // ONNX/MiniLM and ignores the cloud config), so we DO NOT call warmup — we
    // drive the bridge directly. CRITICAL: AFT's config trust boundary
    // (config_resolve.rs) DROPS an inline `semantic.backend`/`base_url` passed
    // as flat configure params — those credentials/endpoints must arrive as a
    // USER-TIER config doc. So when AFT_USER_CONFIG points at a qwen aft.jsonc,
    // we pass it as `config: [{tier:"user",...}]` + cortexkit_user_config_path,
    // exactly like the runtime arm / DeepSWE seed-builder. Without it (default),
    // we fall back to local ONNX MiniLM via ensureOnnxRuntime.
    const userConfigPath = process.env.AFT_USER_CONFIG;
    let userConfigDoc: string | undefined;
    let semanticFlat: Record<string, unknown> | undefined;
    let ortDir: string | undefined;
    if (userConfigPath && existsSync(userConfigPath)) {
      userConfigDoc = readFileSync(userConfigPath, "utf-8");
      // The bridge TS (bridge.ts spawnProcess) keys cloud-vs-fastembed off the
      // FLAT `semantic.backend` param, while the Rust trust boundary
      // (config_resolve.rs) only honors creds/endpoints from the USER-TIER doc.
      // So we must pass BOTH — flat semantic to pick the cloud spawn path, and
      // the user-tier doc to actually carry the backend/base_url/key past the
      // trust boundary. (Identical to the DeepSWE seed-builder recipe.)
      try {
        const parsed = JSON.parse(userConfigDoc) as { semantic?: Record<string, unknown> };
        if (parsed.semantic) semanticFlat = { ...parsed.semantic, max_files: 200000 };
      } catch {
        /* doc may be JSONC; the user-tier path still carries it to Rust */
      }
    } else {
      ortDir = process.env.ORT_DYLIB_PATH
        ? dirname(process.env.ORT_DYLIB_PATH)
        : await ensureOnnxRuntime(storageDir);
    }

    const baseConfig: Record<string, unknown> = {
      harness: "opencode",
      storage_dir: storageDir,
      indexes: { trigram: true, semantic: true },
      restrict_to_project_root: false,
      // Flat semantic: picks the cloud spawn path in the bridge TS.
      ...(semanticFlat ? { semantic: semanticFlat } : {}),
      // User-tier doc: the ONLY channel AFT's Rust honors for cloud creds/endpoint.
      ...(userConfigDoc
        ? {
            cortexkit_user_config_path: userConfigPath,
            config: [{ tier: "user", source: userConfigPath, doc: userConfigDoc }],
          }
        : {}),
      ...(ortDir ? { _ort_dylib_dir: ortDir } : {}),
    };

    this.bridge = new BinaryBridge(
      this.context.aftBinary,
      this.context.codebasePath,
      { timeoutMs: this.context.timeoutMs, errorPrefix: "[aft-vs-codegraph-retrieval]" },
      baseConfig,
    );
    const response = await this.bridge.send(
      "configure",
      { project_root: this.context.codebasePath, ...baseConfig },
      { timeoutMs: this.context.timeoutMs },
    );
    assertBridgeSuccess("configure", response);
    await this.waitForReady();
  }

  async run(testCase: RetrievalCase): Promise<{ items: RankedResult[] }> {
    const response = await this.send("semantic_search", {
      query: testCase.query,
      top_k: this.context.topK,
      hint: testCase.hint ?? (testCase.mode === "search" ? "semantic" : "auto"),
    });
    assertBridgeSuccess("semantic_search", response);
    const results = Array.isArray(response.results) ? response.results : [];
    return {
      items: results.slice(0, this.context.topK).map((result, index) =>
        aftResultToItem(result, index + 1, this.context.codebasePath),
      ),
    };
  }

  async close(): Promise<void> {
    await this.bridge?.shutdown();
    this.bridge = null;
  }

  private async waitForReady(): Promise<void> {
    const deadline = performance.now() + this.context.timeoutMs;
    let lastStatus: BridgeResponse = {};
    while (performance.now() < deadline) {
      const response = await this.send("status", {});
      assertBridgeSuccess("status", response);
      lastStatus = response;
      const searchStatus = nestedStatus(response.search_index);
      const semanticStatus = nestedStatus(response.semantic_index);
      if (searchStatus === "failed" || semanticStatus === "failed") {
        throw new Error(`AFT index failed: ${JSON.stringify(response)}`);
      }
      if ((searchStatus === "ready" || searchStatus === undefined) && semanticStatus === "ready") return;
      await Bun.sleep(500);
    }
    throw new Error(`AFT indexes did not become ready: ${JSON.stringify(lastStatus)}`);
  }

  private async send(command: string, params: Record<string, unknown>): Promise<BridgeResponse> {
    if (!this.bridge) throw new Error("AFT bridge is not prepared");
    return this.bridge.send(command, params, { timeoutMs: this.context.timeoutMs });
  }
}

class CodeGraphRetrievalDriver implements RetrievalDriver {
  readonly name = "codegraph" as const;

  constructor(private readonly context: DriverContext) {}

  async prepare(): Promise<void> {
    const status = await runCommand(
      ["codegraph", "status", this.context.codebasePath, "--json"],
      HARNESS_DIR,
      this.context.timeoutMs,
      codegraphEnv(),
    );
    const initialized = status.exitCode === 0 && statusReportsInitialized(status.stdout);
    if (!initialized) {
      const init = await runCommand(
        ["codegraph", "init", this.context.codebasePath],
        HARNESS_DIR,
        this.context.timeoutMs,
        codegraphEnv(),
      );
      if (init.exitCode !== 0) throw new Error(`codegraph init failed: ${init.stderr || init.stdout}`);
    }
    const index = await runCommand(
      ["codegraph", "index", this.context.codebasePath, "--quiet"],
      HARNESS_DIR,
      this.context.timeoutMs,
      codegraphEnv(),
    );
    if (index.exitCode !== 0) throw new Error(`codegraph index failed: ${index.stderr || index.stdout}`);
  }

  async run(testCase: RetrievalCase): Promise<{ items: RankedResult[] }> {
    if (testCase.mode === "search") return this.runSearch(testCase);
    return this.runContext(testCase);
  }

  private async runSearch(testCase: RetrievalCase): Promise<{ items: RankedResult[] }> {
    const args = [
      "codegraph",
      "query",
      testCase.query,
      "--path",
      this.context.codebasePath,
      "--limit",
      String(this.context.topK),
      "--json",
    ];
    if (testCase.kind) args.push("--kind", normalizeCodeGraphKind(testCase.kind));
    const result = await runCommand(args, HARNESS_DIR, this.context.timeoutMs, codegraphEnv());
    if (result.exitCode !== 0) throw new Error(result.stderr || result.stdout || "codegraph query failed");
    const parsed = JSON.parse(result.stdout || "[]") as unknown[];
    return {
      items: parsed.slice(0, this.context.topK).map((row, index) =>
        codegraphSearchRowToItem(row, index + 1, this.context.codebasePath),
      ),
    };
  }

  private async runContext(testCase: RetrievalCase): Promise<{ items: RankedResult[] }> {
    // CodeGraph 1.x removed `context`; `explore` is its renamed successor (same
    // codegraph_explore MCP tool). It has no --json/--format, so we parse its
    // markdown: a blast-radius bullet list of matched symbols, then a Source
    // Code section with per-file headers listing the symbols in each file.
    const result = await runCommand(
      [
        "codegraph",
        "explore",
        testCase.query,
        "--path",
        this.context.codebasePath,
        "--max-files",
        String(Math.max(this.context.topK, 20)),
      ],
      HARNESS_DIR,
      this.context.timeoutMs,
      codegraphEnv(),
    );
    if (result.exitCode !== 0) throw new Error(result.stderr || result.stdout || "codegraph explore failed");
    return { items: exploreMarkdownToItems(result.stdout, this.context.topK, this.context.codebasePath) };
  }
}

function statusReportsInitialized(stdout: string): boolean {
  try {
    const parsed = JSON.parse(stdout) as { initialized?: boolean };
    return parsed.initialized === true;
  } catch {
    return false;
  }
}

function aftResultToItem(result: unknown, rank: number, codebasePath: string): RankedResult {
  const row = plainObject(result);
  return {
    rank,
    file: normalizePath(stringValue(row.file) ?? stringValue(row.file_path), codebasePath),
    symbol: stringValue(row.name),
    kind: stringValue(row.kind),
    startLine: numberValue(row.start_line) ?? numberValue(row.line_start),
    endLine: numberValue(row.end_line) ?? numberValue(row.line_end),
    score: numberValue(row.score),
    text: stringValue(row.snippet) ?? stringValue(row.text),
  };
}

function codegraphSearchRowToItem(row: unknown, rank: number, codebasePath: string): RankedResult {
  const outer = plainObject(row);
  const node = plainObject(outer.node);
  return {
    rank,
    file: normalizePath(stringValue(node.filePath) ?? stringValue(node.file_path), codebasePath),
    symbol: stringValue(node.name),
    kind: stringValue(node.kind),
    startLine: numberValue(node.startLine) ?? numberValue(node.start_line),
    endLine: numberValue(node.endLine) ?? numberValue(node.end_line),
    score: numberValue(outer.score),
    text: [stringValue(node.signature), stringValue(node.name)].filter(Boolean).join("\n"),
  };
}

/**
 * Parse `codegraph explore` markdown into ranked (symbol, file) results.
 * Explore output has two structured regions we exploit (both name relevant
 * symbols + their files, which is exactly what the ground truth matches on):
 *
 *   1. Blast-radius bullets — CodeGraph's explicit "relevant symbols" answer:
 *        - `BinaryBridge` (src/bridge.ts:1) — 1 caller in `src/bridge.ts`; ...
 *   2. Source-Code file headers — the verbatim-source files it surfaced, with
 *      the symbols present in each:
 *        **`src/bridge.ts`** — BinaryBridge(class), send(method), helper(function)
 *
 * Blast-radius hits rank first (its primary relevance ranking), then
 * source-dump symbols in file/listing order.
 */
function exploreMarkdownToItems(markdown: string, topK: number, codebasePath: string): RankedResult[] {
  const items: RankedResult[] = [];
  const seen = new Set<string>();
  const push = (symbol: string | undefined, file: string | undefined, line: number | undefined, text: string) => {
    const normFile = file ? normalizePath(resolve(codebasePath, file), codebasePath) : undefined;
    const key = `${symbol ?? ""}@${normFile ?? ""}`;
    if (!symbol && !normFile) return;
    if (seen.has(key)) return;
    seen.add(key);
    items.push({ rank: items.length + 1, symbol, file: normFile, startLine: line, text });
  };

  // 1. Blast-radius bullets: - `Symbol` (path/file.ext:line) — ...
  const blast = /^[-*]\s+`([A-Za-z_$][\w$.:]*)`\s+\(([^():]+?):(\d+)\)/gm;
  for (const m of markdown.matchAll(blast)) {
    push(m[1], m[2], Number.parseInt(m[3]!, 10), surroundingLine(markdown, m.index ?? 0));
    if (items.length >= topK) return items;
  }

  // 2. Source-Code file headers: **`path/file.ext`** — Sym(kind), Sym(kind), ...
  const fileHeader = /^\*\*`([^`]+)`\*\*\s*(?:—|-)?\s*(.*)$/gm;
  for (const m of markdown.matchAll(fileHeader)) {
    const file = m[1]!;
    const symbolList = m[2] ?? "";
    for (const sym of symbolList.matchAll(/([A-Za-z_$][\w$]*)\s*\([a-z]+\)/g)) {
      push(sym[1], file, undefined, `${sym[1]} in ${file}`);
      if (items.length >= topK) return items;
    }
    // header with no inline symbol list still counts as a file hit
    if (!symbolList.trim()) push(undefined, file, undefined, file);
    if (items.length >= topK) return items;
  }
  return items;
}

function surroundingLine(text: string, index: number): string {
  const start = text.lastIndexOf("\n", index) + 1;
  const end = text.indexOf("\n", index);
  return text.slice(start, end === -1 ? undefined : end).trim();
}

function normalizeCodeGraphKind(kind: string): string {
  if (kind === "interface") return "interface";
  if (kind === "enum") return "enum";
  return kind;
}

function codegraphEnv(): Record<string, string> {
  return {
    CI: "1",
    CODEGRAPH_NO_WATCH: "1",
    CODEGRAPH_NO_DAEMON: "1",
    CODEGRAPH_WATCH_DEBOUNCE_MS: "100",
  };
}

function assertBridgeSuccess(command: string, response: BridgeResponse): void {
  if (response.success === false) {
    throw new Error(`${command} failed: ${stringValue(response.message) ?? JSON.stringify(response)}`);
  }
}

function nestedStatus(value: unknown): string | undefined {
  return stringValue(plainObject(value).status);
}

function plainObject(value: unknown): PlainObject {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as PlainObject) : {};
}

function stringValue(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

function numberValue(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}
