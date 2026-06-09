import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { BinaryBridge } from "../../../packages/aft-bridge/src/index.ts";
import type {
  AftTool,
  DriverContext,
  DriverRunResult,
  EvalDriver,
  EvalTestCase,
  RankedResult,
} from "./types";
import { escapeRegexLiteral, listProjectFiles, normalizePath, runCommand } from "./util";

type BridgeResponse = Record<string, unknown>;

export function createDriver(name: string, context: DriverContext): EvalDriver {
  if (name === "aft") return new AftBridgeDriver(context, "aft_search");
  if (name === "aft-grep") return new AftBridgeDriver(context, "aft_grep");
  if (name === "ripgrep") return new RipgrepDriver(context);
  if (name === "list-files") return new ListFilesDriver(context);
  throw new Error(`Unknown driver: ${name}`);
}

class AftBridgeDriver implements EvalDriver {
  name: string;
  private bridge: BinaryBridge | null = null;

  constructor(
    private readonly context: DriverContext,
    private readonly defaultTool: AftTool,
  ) {
    this.name = defaultTool === "aft_grep" ? "aft-grep" : "aft";
  }

  async prepare(): Promise<void> {
    if (!existsSync(this.context.binaryPath)) {
      throw new Error(`AFT binary not found: ${this.context.binaryPath}`);
    }
    this.bridge = new BinaryBridge(
      this.context.binaryPath,
      this.context.codebasePath,
      { timeoutMs: this.context.readyTimeoutMs, errorPrefix: "[aft-codegraph-bench]" },
      {
        harness: "opencode",
        search_index: true,
        semantic_search: true,
        experimental_search_index: true,
        experimental_semantic_search: true,
      },
    );
    const response = await this.bridge.send(
      "configure",
      {
        project_root: this.context.codebasePath,
        harness: "opencode",
        search_index: true,
        semantic_search: true,
        experimental_search_index: true,
        experimental_semantic_search: true,
      },
      { timeoutMs: this.context.readyTimeoutMs },
    );
    assertSuccess("configure", response);
    await this.waitForReady(this.defaultTool === "aft_search");
  }

  async run(testCase: EvalTestCase): Promise<DriverRunResult> {
    const tool = this.defaultTool === "aft_grep" ? "aft_grep" : (testCase.tool ?? this.defaultTool);
    if (tool === "aft_search") return this.runSemanticSearch(testCase);
    if (tool === "aft_grep") return this.runGrep(testCase);
    if (tool === "aft_outline") return this.runOutline(testCase);
    if (tool === "aft_zoom") return this.runZoom(testCase);
    if (tool === "aft_navigate") return this.runNavigate(testCase);
    throw new Error(`AFT bridge driver cannot run tool ${tool}`);
  }

  async close(): Promise<void> {
    await this.bridge?.shutdown();
    this.bridge = null;
  }

  private async runSemanticSearch(testCase: EvalTestCase): Promise<DriverRunResult> {
    const limit =
      numberOption(testCase, "searchLimit") ?? numberOption(testCase, "topK") ?? this.context.topK;
    const response = await this.send("semantic_search", {
      query: testCase.query,
      top_k: limit,
    });
    assertSuccess("semantic_search", response);
    const results = Array.isArray(response.results) ? response.results : [];
    return {
      items: results
        .slice(0, this.context.topK)
        .map((result, index) => semanticResultToItem(result, index + 1, this.context.codebasePath)),
      raw: compactRaw(response),
      status: stringValue(response.status),
    };
  }

  private async runGrep(testCase: EvalTestCase): Promise<DriverRunResult> {
    const response = await this.send("grep", {
      pattern: escapeRegexLiteral(testCase.query),
      case_sensitive: true,
      max_results: this.context.topK,
      path: stringOption(testCase, "path"),
    });
    assertSuccess("grep", response);
    const matches = Array.isArray(response.matches) ? response.matches : [];
    return {
      items: matches
        .slice(0, this.context.topK)
        .map((match, index) => grepMatchToItem(match, index + 1, this.context.codebasePath)),
      raw: compactRaw(response),
    };
  }

  private async runOutline(testCase: EvalTestCase): Promise<DriverRunResult> {
    const file = stringOption(testCase, "file") ?? stringOption(testCase, "target");
    const directory = stringOption(testCase, "directory");
    const filesMode = Boolean(testCase.options?.files);
    const params: Record<string, unknown> = filesMode
      ? { directory: directory ?? file ?? ".", files: true }
      : directory
        ? { directory }
        : { file: file ?? "." };
    const response = await this.send("outline", params);
    assertSuccess("outline", response);
    const text = stringValue(response.text) ?? JSON.stringify(response);
    return {
      items: textToItems(text, this.context.topK, file, this.context.codebasePath),
      raw: compactRaw(response),
    };
  }

  private async runZoom(testCase: EvalTestCase): Promise<DriverRunResult> {
    const file = requireStringOption(testCase, "file");
    const symbol = requireStringOption(testCase, "symbol");
    const response = await this.send("zoom", {
      file,
      symbol,
      context_lines: numberOption(testCase, "contextLines") ?? 3,
    });
    assertSuccess("zoom", response);
    return {
      items: [zoomResponseToItem(response, file, this.context.codebasePath)],
      raw: compactRaw(response),
    };
  }

  private async runNavigate(testCase: EvalTestCase): Promise<DriverRunResult> {
    const op = requireStringOption(testCase, "op");
    const command = navigateCommand(op);
    const response = await this.send(command, {
      file: requireStringOption(testCase, "file"),
      symbol: requireStringOption(testCase, "symbol"),
      depth: numberOption(testCase, "depth") ?? 5,
      expression: stringOption(testCase, "expression"),
      to_symbol: stringOption(testCase, "toSymbol"),
      to_file: stringOption(testCase, "toFile"),
    });
    assertSuccess(command, response);
    const text = stringValue(response.text) ?? JSON.stringify(response, null, 2);
    return {
      items: textToItems(
        text,
        this.context.topK,
        stringOption(testCase, "file"),
        this.context.codebasePath,
      ),
      raw: compactRaw(response),
    };
  }

  private async waitForReady(requireSemantic: boolean): Promise<void> {
    const deadline = performance.now() + this.context.readyTimeoutMs;
    let lastStatus: BridgeResponse = {};
    while (performance.now() < deadline) {
      const response = await this.send("status", {});
      assertSuccess("status", response);
      lastStatus = response;
      const searchStatus = nestedStatus(response.search_index);
      const semanticStatus = nestedStatus(response.semantic_index);
      if (searchStatus === "failed" || semanticStatus === "failed") {
        throw new Error(`AFT index failed: ${JSON.stringify(response)}`);
      }
      const searchReady = searchStatus === "ready" || searchStatus === undefined;
      const semanticReady = semanticStatus === "ready";
      if (searchReady && (!requireSemantic || semanticReady)) return;
      await Bun.sleep(500);
    }
    throw new Error(`AFT indexes did not become ready: ${JSON.stringify(lastStatus)}`);
  }

  private async send(command: string, params: Record<string, unknown>): Promise<BridgeResponse> {
    if (!this.bridge) throw new Error("AFT bridge is not prepared");
    return this.bridge.send(command, params, { timeoutMs: this.context.readyTimeoutMs });
  }
}

class RipgrepDriver implements EvalDriver {
  name = "ripgrep";

  constructor(private readonly context: DriverContext) {}

  async run(testCase: EvalTestCase): Promise<DriverRunResult> {
    const command = [
      "rg",
      "-n",
      "--no-heading",
      "--color",
      "never",
      "-F",
      "--glob",
      "!.git/**",
      "--glob",
      "!node_modules/**",
      "--glob",
      "!target/**",
      "--glob",
      "!dist/**",
      "--",
      testCase.query,
      ".",
    ];
    const result = await runCommand(
      command,
      this.context.codebasePath,
      this.context.readyTimeoutMs,
    );
    if (result.timedOut) throw new Error("ripgrep timed out");
    if (result.exitCode > 1) throw new Error(result.stderr || `ripgrep exited ${result.exitCode}`);
    return {
      items: parseRipgrep(result.stdout, this.context.topK, this.context.codebasePath),
      raw: { exitCode: result.exitCode, stderr: result.stderr.trim() },
    };
  }
}

class ListFilesDriver implements EvalDriver {
  name = "list-files";

  constructor(private readonly context: DriverContext) {}

  async run(): Promise<DriverRunResult> {
    return {
      items: listProjectFiles(this.context.codebasePath, this.context.topK).map((file, index) => ({
        rank: index + 1,
        file,
        name: file.split("/").pop(),
        kind: "file",
        text: file,
      })),
    };
  }
}

function semanticResultToItem(result: unknown, rank: number, codebasePath: string): RankedResult {
  const row = plainObject(result);
  return {
    rank,
    file: normalizePath(stringValue(row.file), codebasePath),
    name: stringValue(row.name),
    kind: stringValue(row.kind),
    line: numberValue(row.start_line),
    endLine: numberValue(row.end_line),
    score: numberValue(row.score),
    source: stringValue(row.source),
    text: stringValue(row.snippet),
  };
}

function grepMatchToItem(match: unknown, rank: number, codebasePath: string): RankedResult {
  const row = plainObject(match);
  const text = stringValue(row.line_text) ?? stringValue(row.text) ?? "";
  return {
    rank,
    file: normalizePath(stringValue(row.file), codebasePath),
    line: numberValue(row.line),
    kind: "line",
    text,
  };
}

function zoomResponseToItem(
  response: BridgeResponse,
  file: string,
  codebasePath: string,
): RankedResult {
  return {
    rank: 1,
    file: normalizePath(file, codebasePath),
    name: stringValue(response.name),
    kind: stringValue(response.kind),
    line: numberValue(plainObject(response.range).start_line),
    endLine: numberValue(plainObject(response.range).end_line),
    text: [
      stringValue(response.content),
      ...arrayOfStrings(response.context_before),
      ...arrayOfStrings(response.context_after),
    ]
      .filter(Boolean)
      .join("\n"),
  };
}

function textToItems(
  text: string,
  topK: number,
  file: string | undefined,
  codebasePath: string,
): RankedResult[] {
  const items: RankedResult[] = [];
  for (const line of text.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    items.push({
      rank: items.length + 1,
      file: file ? normalizePath(file, codebasePath) : undefined,
      name: parseSymbolName(trimmed),
      kind: parseSymbolKind(trimmed),
      text: trimmed,
    });
    if (items.length >= topK) break;
  }
  return items;
}

function parseRipgrep(stdout: string, topK: number, codebasePath: string): RankedResult[] {
  const items: RankedResult[] = [];
  for (const line of stdout.split("\n")) {
    if (!line.trim()) continue;
    const match = /^(.*?):(\d+):(.*)$/.exec(line);
    if (!match) continue;
    items.push({
      rank: items.length + 1,
      file: normalizePath(resolve(codebasePath, match[1]), codebasePath),
      line: Number.parseInt(match[2], 10),
      kind: "line",
      text: match[3],
    });
    if (items.length >= topK) break;
  }
  return items;
}

function parseSymbolName(line: string): string | undefined {
  const signatureMatch =
    /(?:pub\s+)?(?:fn|struct|enum|interface|class|type|method|mth)\s+([A-Za-z_$][\w$]*)/.exec(line);
  if (signatureMatch) return signatureMatch[1];
  const bracketMatch = /^([A-Za-z_$][\w$]*)\s+\[/.exec(line);
  if (bracketMatch) return bracketMatch[1];
  return undefined;
}

function parseSymbolKind(line: string): string | undefined {
  const match =
    /\b(fn|function|struct|enum|interface|class|method|mth|type_alias|file-summary|line)\b/.exec(
      line,
    );
  return match?.[1];
}

function navigateCommand(op: string): string {
  const commands: Record<string, string> = {
    callers: "callers",
    call_tree: "call_tree",
    trace_to: "trace_to",
    trace_to_symbol: "trace_to_symbol",
    impact: "impact",
    trace_data: "trace_data",
  };
  const command = commands[op];
  if (!command) throw new Error(`Unsupported aft_navigate op: ${op}`);
  return command;
}

function numberOption(testCase: EvalTestCase, key: string): number | undefined {
  return numberValue(testCase.options?.[key]);
}

function stringOption(testCase: EvalTestCase, key: string): string | undefined {
  return stringValue(testCase.options?.[key]);
}

function requireStringOption(testCase: EvalTestCase, key: string): string {
  const value = stringOption(testCase, key);
  if (!value) throw new Error(`Case ${testCase.id} requires options.${key}`);
  return value;
}

function assertSuccess(command: string, response: BridgeResponse): void {
  if (response.success === false) {
    throw new Error(
      `${command} failed: ${stringValue(response.message) ?? JSON.stringify(response)}`,
    );
  }
}

function compactRaw(response: BridgeResponse): BridgeResponse {
  const copy = { ...response };
  delete copy.results;
  delete copy.matches;
  delete copy.text;
  delete copy.content;
  return copy;
}

function nestedStatus(value: unknown): string | undefined {
  return stringValue(plainObject(value).status);
}

function plainObject(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

function stringValue(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

function numberValue(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function arrayOfStrings(value: unknown): string[] {
  return Array.isArray(value)
    ? value.filter((item): item is string => typeof item === "string")
    : [];
}
