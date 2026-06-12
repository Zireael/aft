#!/usr/bin/env bun
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { parseArgs } from "node:util";
import type { AgentArm, AgentCheck, AgentCorpus, AgentReport, AgentRunResult, AgentTask, ArmConfig, TokenBreakdown, TokenUsage } from "./types";
import { copyFixture, ensureExists, HARNESS_DIR, percentile, readOpencodeAuthKey, resetDir, round, runCommand, writeJson } from "./util";

interface CliOptions {
  corpusPath: string;
  arms: AgentArm[];
  model: string;
  fallbackModel: string;
  outDir: string;
  runRoot: string;
  limit?: number;
  taskIds: string[];
  timeoutMs: number;
  dryRun: boolean;
  providerBaseUrl?: string;
  providerName?: string;
  providerApiKey?: string;
}

async function main(): Promise<void> {
  const options = parseCliArgs();
  const corpus = loadCorpus(options.corpusPath);
  const fixturePath = resolve(HARNESS_DIR, corpus.fixturePath);
  ensureExists(fixturePath, "fixture");

  const selectedTasks = selectTasks(corpus.tasks, options.taskIds, options.limit);
  resetDir(options.runRoot);
  mkdirSync(options.outDir, { recursive: true });
  // Bug #1 fix: clear stale transcripts at the start of every run so artifacts
  // from a previous run can never be mistaken for fresh data. The transcripts
  // dir lives under outDir (mounted to the host), so fresh transcripts always
  // refresh on the host.
  const transcriptsDir = resolve(options.outDir, "transcripts");
  resetDir(transcriptsDir);

  const results: AgentRunResult[] = [];
  for (const task of selectedTasks) {
    for (const arm of options.arms) {
      console.log(`\n[${arm}] ${task.id}`);
      results.push(await runTaskArm(task, arm, fixturePath, options));
      const last = results[results.length - 1]!;
      console.log(`  ${last.success ? "PASS" : "FAIL"} ${round(last.wallTimeMs / 1000, 1)}s tokens=${last.tokens.total} tools=${last.toolCalls}${last.error ? ` error=${last.error}` : ""}`);
    }
  }

  const report = buildReport(corpus, selectedTasks, options, results);
  const stamp = new Date(report.timestamp).toISOString().replace(/[:.]/g, "-");
  const jsonPath = resolve(options.outDir, `agent-ab-${stamp}.json`);
  const mdPath = resolve(options.outDir, `agent-ab-${stamp}.md`);
  writeFileSync(jsonPath, `${JSON.stringify(report, null, 2)}\n`);
  writeFileSync(mdPath, markdownSummary(report));
  console.log(`\nJSON report: ${jsonPath}`);
  console.log(`Markdown summary: ${mdPath}`);
}

async function runTaskArm(task: AgentTask, arm: AgentArm, fixturePath: string, options: CliOptions): Promise<AgentRunResult> {
  const runDir = resolve(options.runRoot, task.id, arm);
  const repoPath = resolve(runDir, "repo");
  const configRoot = resolve(runDir, "config");
  const stdoutPath = resolve(runDir, "opencode.stdout.jsonl");
  const stderrPath = resolve(runDir, "opencode.stderr.log");
  mkdirSync(runDir, { recursive: true });
  copyFixture(fixturePath, repoPath);
  await prepareArm(arm, repoPath, configRoot, options.timeoutMs, options.dryRun, options.providerBaseUrl, options.providerName);

  const attemptedModel = options.model;
  let model = attemptedModel;
  const started = performance.now();
  let stdout = "";
  let stderr = "";
  let exitCode = 0;
  let error: string | undefined;

  try {
    if (options.dryRun) {
      stdout = dryRunOutput(task);
      stderr = "dry-run: OpenCode was not invoked\n";
      applyDryRunEdit(task, repoPath);
    } else {
      const first = await invokeOpencode(task, repoPath, configRoot, model, options.timeoutMs);
      stdout = first.stdout;
      stderr = first.stderr;
      exitCode = first.exitCode;
      if (isRateLimited(stdout, stderr, exitCode) && model === options.model) {
        model = options.fallbackModel;
        const retry = await invokeOpencode(task, repoPath, configRoot, model, options.timeoutMs);
        stdout += `\n{\"benchmark_retry\":\"rate_limited\",\"fallback_model\":\"${model}\"}\n` + retry.stdout;
        stderr += `\n--- retry with ${model} ---\n` + retry.stderr;
        exitCode = retry.exitCode;
      }
    }
  } catch (err) {
    exitCode = 1;
    error = err instanceof Error ? err.message : String(err);
    stderr += `\n${error}\n`;
  }

  mkdirSync(dirname(stdoutPath), { recursive: true });
  writeFileSync(stdoutPath, stdout);
  writeFileSync(stderrPath, stderr);

  // Bug #1 fix: render a fresh human-readable transcript into the mounted
  // outDir/transcripts/ for THIS run, so analysis never reads stale artifacts.
  const transcriptsDir = resolve(options.outDir, "transcripts");
  mkdirSync(transcriptsDir, { recursive: true });
  const transcriptPath = resolve(transcriptsDir, `${task.id}.${arm}.txt`);
  writeFileSync(transcriptPath, renderTranscript(stdout));

  const wallTimeMs = performance.now() - started;
  const answerText = extractVisibleText(stdout);
  const checks = await evaluateChecks(task.checks, repoPath, answerText || stdout, options.timeoutMs);
  const success = exitCode === 0 && checks.every((check) => check.pass) && !error;
  const tokens = parseTokenUsage(stdout);
  const tokenBreakdown = parseTokenBreakdown(stdout);
  return {
    arm,
    taskId: task.id,
    kind: task.kind,
    success,
    model,
    attemptedModel,
    wallTimeMs: round(wallTimeMs, 3),
    exitCode,
    tokens,
    tokenBreakdown,
    toolCalls: countToolCalls(stdout),
    answerText: truncate(answerText || stdout, 4000),
    stdoutPath,
    stderrPath,
    transcriptPath,
    repoPath,
    checks,
    error,
  };
}

async function prepareArm(
  arm: AgentArm,
  repoPath: string,
  configRoot: string,
  timeoutMs: number,
  dryRun: boolean,
  providerBaseUrl?: string,
  providerName?: string,
): Promise<void> {
  const opencodeDir = resolve(configRoot, "opencode");
  mkdirSync(opencodeDir, { recursive: true });
  const provider = providerBaseUrl ? customProviderConfig(providerBaseUrl, providerName) : zenProviderConfig();

  if (arm === "aft") {
    writeJson(resolve(opencodeDir, "opencode.json"), {
      $schema: "https://opencode.ai/config.json",
      plugin: [AFT_PLUGIN_SPEC],
      provider,
    });
    writeFileSync(resolve(opencodeDir, "aft.jsonc"), `${JSON.stringify(aftBenchConfig(), null, 2)}\n`);
    if (!dryRun) {
      const storageDir = cortexKitStorageRoot();
      const warmup = await runCommand(
        ["aft", "warmup", "--root", repoPath, "--timeout", String(timeoutMs), "--quiet"],
        HARNESS_DIR,
        timeoutMs,
        {
          AFT_STORAGE_DIR: storageDir,
          FASTEMBED_CACHE_DIR: join(storageDir, "semantic", "models"),
        },
      );
      if (warmup.exitCode !== 0) {
        throw new Error(`aft warmup failed: ${warmup.stderr || warmup.stdout}`);
      }
    }
    return;
  }

  writeJson(resolve(opencodeDir, "opencode.json"), {
    $schema: "https://opencode.ai/config.json",
    provider,
    mcp: codegraphMcpConfig(repoPath),
  });

  const init = await runCommand(["codegraph", "init", repoPath], HARNESS_DIR, timeoutMs, codegraphEnv());
  if (init.exitCode !== 0 && !`${init.stderr}\n${init.stdout}`.includes("Already initialized")) {
    throw new Error(`codegraph init failed: ${init.stderr || init.stdout}`);
  }
  const index = await runCommand(["codegraph", "index", repoPath, "--quiet"], HARNESS_DIR, timeoutMs, codegraphEnv());
  if (index.exitCode !== 0) throw new Error(`codegraph index failed: ${index.stderr || index.stdout}`);
}

async function invokeOpencode(task: AgentTask, repoPath: string, configRoot: string, model: string, timeoutMs: number): Promise<{ stdout: string; stderr: string; exitCode: number }> {
  const usingCustomProvider = nonEmpty(process.env.AGENT_PROVIDER_BASE_URL);
  const apiKey = usingCustomProvider
    ? nonEmpty(process.env.AGENT_PROVIDER_API_KEY) ?? "sk-benchmark-noop"
    : nonEmpty(process.env.OPENCODE_API_KEY) ?? nonEmpty(process.env.OPENAI_API_KEY) ?? readOpencodeAuthKey("opencode-go");
  if (!usingCustomProvider && !apiKey) {
    throw new Error("Missing opencode-go auth. Mount ~/.local/share/opencode/auth.json or set OPENCODE_API_KEY.");
  }
  const prompt = benchmarkPrompt(task);
  const storageDir = cortexKitStorageRoot();
  const result = await runCommand(
    [
      "opencode",
      "run",
      "--format",
      "json",
      "--model",
      model,
      "--dir",
      repoPath,
      "--dangerously-skip-permissions",
      prompt,
    ],
    repoPath,
    task.timeoutMs ?? timeoutMs,
    {
      XDG_CONFIG_HOME: configRoot,
      XDG_DATA_HOME: cortexKitDataHome(),
      AFT_STORAGE_DIR: storageDir,
      AFT_WAIT_FOR_SEMANTIC_READY: "1",
      AFT_WAIT_FOR_SEMANTIC_READY_MS: String(timeoutMs),
      FASTEMBED_CACHE_DIR: join(storageDir, "semantic", "models"),
      OPENAI_API_KEY: apiKey ?? undefined,
      OPENCODE_API_KEY: apiKey ?? undefined,
      AFT_BENCHMARK: "1",
    },
  );
  return { stdout: result.stdout, stderr: result.stderr, exitCode: result.exitCode };
}

async function evaluateChecks(checks: AgentCheck[], repoPath: string, answerText: string, timeoutMs: number): Promise<Array<{ check: AgentCheck; pass: boolean; detail?: string }>> {
  const results = [];
  for (const check of checks) {
    if (check.type === "answer_contains") {
      const haystack = answerText.toLowerCase();
      const missing = check.values.filter((value) => !haystack.includes(value.toLowerCase()));
      results.push({ check, pass: missing.length === 0, detail: missing.length ? `missing: ${missing.join(", ")}` : undefined });
    } else if (check.type === "file_contains") {
      const path = resolve(repoPath, check.path);
      const content = existsSync(path) ? readFileSync(path, "utf8") : "";
      results.push({ check, pass: content.includes(check.value), detail: content.includes(check.value) ? undefined : `${check.path} does not contain expected text` });
    } else if (check.type === "file_not_contains") {
      const path = resolve(repoPath, check.path);
      const content = existsSync(path) ? readFileSync(path, "utf8") : "";
      results.push({ check, pass: !content.includes(check.value), detail: !content.includes(check.value) ? undefined : `${check.path} contains forbidden text` });
    } else {
      const command = check.command.split(/\s+/).filter(Boolean);
      const result = await runCommand(command, repoPath, timeoutMs);
      results.push({ check, pass: result.exitCode === 0, detail: result.exitCode === 0 ? undefined : result.stderr || result.stdout });
    }
  }
  return results;
}

function loadCorpus(path: string): AgentCorpus {
  const fullPath = resolve(HARNESS_DIR, path);
  const corpus = JSON.parse(readFileSync(fullPath, "utf8")) as AgentCorpus;
  if (!Array.isArray(corpus.tasks) || corpus.tasks.length === 0) throw new Error(`No tasks in ${fullPath}`);
  return corpus;
}

function selectTasks(tasks: AgentTask[], taskIds: string[], limit: number | undefined): AgentTask[] {
  let selected = taskIds.length > 0 ? tasks.filter((task) => taskIds.includes(task.id)) : tasks;
  if (limit) selected = selected.slice(0, limit);
  if (selected.length === 0) throw new Error("No tasks selected");
  return selected;
}

function buildReport(corpus: AgentCorpus, tasks: AgentTask[], options: CliOptions, results: AgentRunResult[]): AgentReport {
  const summary: AgentReport["summary"] = { aft: emptySummary(), codegraph: emptySummary() };
  for (const arm of options.arms) {
    const armResults = results.filter((result) => result.arm === arm);
    const successes = armResults.filter((result) => result.success).length;
    const tokenTotals = armResults.map((result) => result.tokens.total);
    const wallTimes = armResults.map((result) => result.wallTimeMs);
    const toolCalls = armResults.map((result) => result.toolCalls);
    summary[arm] = {
      runs: armResults.length,
      successRate: armResults.length ? round(successes / armResults.length) : 0,
      successes,
      tokensTotal: tokenTotals.reduce((sum, value) => sum + value, 0),
      tokensMedian: percentile(tokenTotals, 50),
      wallTimeMsMedian: percentile(wallTimes, 50),
      wallTimeMsP95: percentile(wallTimes, 95),
      toolCallsMedian: percentile(toolCalls, 50),
    };
  }
  // Bug #2 fix: record the resolved per-arm config so every run is
  // self-describing and two runs can be confirmed to use the same setup.
  const armConfigs: Record<AgentArm, ArmConfig> = {
    aft: resolveArmConfig("aft"),
    codegraph: resolveArmConfig("codegraph"),
  };

  return {
    benchmark: "codegraph-vs-aft-agent",
    timestamp: new Date().toISOString(),
    model: options.model,
    fallbackModel: options.fallbackModel,
    corpus: corpus.name,
    arms: options.arms,
    taskCount: tasks.length,
    summary,
    results,
    armConfigs,
    metadata: {
      dryRun: options.dryRun,
      fixturePath: corpus.fixturePath,
      providerBaseUrl: options.providerBaseUrl ?? null,
      providerName: options.providerName ?? null,
      timeoutMs: options.timeoutMs,
      // Echo the resolved arm configs into metadata too, so a reader scanning
      // `metadata` alone can confirm the tool surface / disabled tools / model
      // wiring used by each arm without cross-referencing armConfigs.
      armConfigs,
      methodology: "OpenCode CLI runs the same deterministic tasks with either AFT plugin tools or CodeGraph MCP tools. Both arms keep native read/edit/write/grep/glob/bash; the only variable is the code-intelligence layer (aft_* vs codegraph_*). Indexes are pre-warmed per arm before the agent runs. Scoring checks final answers, file contents, and optional commands.",
      auth: "API keys are read from OPENCODE_API_KEY/OPENAI_API_KEY or mounted ~/.local/share/opencode/auth.json; keys are never written to reports.",
    },
  };
}

function markdownSummary(report: AgentReport): string {
  const lines = [
    "# AFT vs CodeGraph agent benchmark",
    "",
    `- Model: \`${report.model}\``,
    `- Fallback model: \`${report.fallbackModel}\``,
    `- Corpus: \`${report.corpus}\``,
    `- Timestamp: ${report.timestamp}`,
    `- Tasks: ${report.taskCount}`,
    `- Dry run: ${String(report.metadata.dryRun)}`,
    "",
    "## Summary",
    "",
    "| arm | runs | successes | success rate | tokens total | median tokens | median wall ms | p95 wall ms | median tool calls |",
    "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ...report.arms.map((arm) => {
      const s = report.summary[arm];
      return `| ${arm} | ${s.runs} | ${s.successes} | ${s.successRate.toFixed(3)} | ${s.tokensTotal} | ${s.tokensMedian.toFixed(0)} | ${s.wallTimeMsMedian.toFixed(0)} | ${s.wallTimeMsP95.toFixed(0)} | ${s.toolCallsMedian.toFixed(0)} |`;
    }),
    "",
    "## Per task",
    "",
    "| task | arm | kind | status | model | wall ms | tokens | prompt in | tool-out tok≈ | tool calls |",
    "| --- | --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: |",
    ...report.results.map((result) => {
      const b = result.tokenBreakdown;
      return `| ${result.taskId} | ${result.arm} | ${result.kind} | ${result.success ? "PASS" : "FAIL"} | ${result.model} | ${result.wallTimeMs.toFixed(0)} | ${result.tokens.total} | ${b?.promptInputTokens ?? 0} | ${b?.toolOutputTokensEst ?? 0} | ${result.toolCalls} |`;
    }),
    "",
    "## Arm configuration (recorded for cross-run comparability)",
    "",
    ...report.arms.flatMap((arm) => {
      const c = report.armConfigs[arm];
      const detail = arm === "aft" ? JSON.stringify(c.aft) : JSON.stringify(c.mcp);
      return [
        `- **${arm}** — intelligence layer: \`${c.intelligenceLayer}\``,
        `  - pre-warm: ${c.preWarm}`,
        `  - shared built-in tools: ${Object.keys(c.builtinTools).join(", ")}`,
        `  - config: \`${detail}\``,
      ];
    }),
    "",
  ];
  return `${lines.join("\n")}\n`;
}

function parseCliArgs(): CliOptions {
  const { values } = parseArgs({
    options: {
      corpus: { type: "string", default: process.env.AGENT_CORPUS ?? "corpora/tasks.json" },
      arms: { type: "string", default: process.env.AGENT_ARMS ?? "aft,codegraph" },
      model: { type: "string", default: process.env.AGENT_MODEL ?? "opencode-go/deepseek-v4-flash-free" },
      "fallback-model": { type: "string", default: process.env.AGENT_FALLBACK_MODEL ?? "opencode-go/deepseek-v4-pro" },
      "out-dir": { type: "string", default: process.env.AGENT_OUT_DIR ?? resolve(HARNESS_DIR, "results") },
      "run-root": { type: "string", default: process.env.AGENT_RUN_ROOT ?? resolve(HARNESS_DIR, ".bench", "runs", new Date().toISOString().replace(/[:.]/g, "-")) },
      limit: { type: "string", default: process.env.AGENT_TASK_LIMIT },
      task: { type: "string", multiple: true },
      "timeout-ms": { type: "string", default: process.env.AGENT_TIMEOUT_MS ?? "240000" },
      "dry-run": { type: "boolean", default: process.env.AGENT_DRY_RUN === "1" },
      "provider-base-url": { type: "string" },
      "provider-name": { type: "string" },
      "provider-api-key": { type: "string" },
      help: { type: "boolean", short: "h" },
    },
    strict: true,
  });
  if (values.help) {
    console.log(`
AFT vs CodeGraph OpenCode agent benchmark

Usage:
  bun run src/cli.ts --limit 2
  AGENT_DRY_RUN=1 bun run src/cli.ts --limit 2

Auth for real runs:
  mount ~/.local/share/opencode/auth.json or set OPENCODE_API_KEY.
`);
    process.exit(0);
  }
  const arms = String(values.arms).split(",").map((arm) => arm.trim()).filter(Boolean) as AgentArm[];
  for (const arm of arms) if (!["aft", "codegraph"].includes(arm)) throw new Error(`Invalid arm: ${arm}`);
  return {
    corpusPath: String(values.corpus),
    arms,
    model: String(values.model),
    fallbackModel: String(values["fallback-model"]),
    outDir: String(values["out-dir"]),
    runRoot: String(values["run-root"]),
    limit: values.limit ? positiveInt(String(values.limit), "--limit") : undefined,
    taskIds: (values.task as string[] | undefined) ?? [],
    timeoutMs: positiveInt(String(values["timeout-ms"]), "--timeout-ms"),
    dryRun: values["dry-run"] === true,
    providerBaseUrl: values["provider-base-url"] ? String(values["provider-base-url"]) : undefined,
    providerName: values["provider-name"] ? String(values["provider-name"]) : undefined,
    providerApiKey: values["provider-api-key"] ? String(values["provider-api-key"]) : undefined,
  };
}

function benchmarkPrompt(task: AgentTask): string {
  return [
    "You are running inside a deterministic benchmark fixture.",
    "Use the available code-intelligence tools for discovery before reading files directly.",
    "Keep the final answer concise. For answer tasks, return only the requested JSON.",
    "For edit tasks, make the requested code change and run the requested test command if asked.",
    "",
    `Task: ${task.prompt}`,
  ].join("\n");
}

function zenProviderConfig(): Record<string, unknown> {
  return {
    "opencode-go": {
      api: "openai",
      name: "opencode-go zen",
      options: { baseURL: "https://opencode.ai/zen/v1" },
      models: {
        "deepseek-v4-flash-free": { name: "deepseek-v4-flash-free" },
        "deepseek-v4-pro": { name: "deepseek-v4-pro" },
      },
    },
  };
}

function customProviderConfig(baseUrl: string, name?: string): Record<string, unknown> {
  const providerId = "local";
  const modelName = name || "default";
  return {
    [providerId]: {
      npm: "@ai-sdk/openai-compatible",
      name: name || "Local LLM",
      options: { baseURL: baseUrl },
      models: {
        [modelName]: { name: modelName },
      },
    },
  };
}

// ── Arm configuration (single source of truth) ──────────────────────────────
// Both arms keep file/edit/grep/glob/bash mechanics IDENTICAL (native OpenCode
// tools) so the only variable under test is the code-intelligence layer.

const AFT_PLUGIN_SPEC = "@cortexkit/aft-opencode@latest";
const CODEGRAPH_PKG = "@colbymchenry/codegraph@0.9.6";

/**
 * Fair-comparison AFT surface. Disables every AFT tool that is NOT the
 * code-intelligence comparator, so AFT's aft_search/aft_outline/aft_zoom/
 * aft_callgraph are pitted against codegraph_* while file/edit/grep/bash stay
 * native on both arms.
 *   - hoist_builtin_tools:false → native read/write/edit (AFT aft_* prefixed; disabled below)
 *   - bash:false                → native bash on both arms (AFT otherwise hoists bash)
 *   - grep/glob disabled         → native grep/glob (search_index stays true so aft_search keeps its lexical lane)
 *   - tool_surface:"all"         → expose aft_callgraph (the codegraph_trace/callers comparator)
 *   - everything non-comparison disabled (safety/import/ast-grep/conflicts/lsp/refactor/transform/move/delete)
 */
function aftBenchConfig(): Record<string, unknown> {
  return {
    search_index: true,
    semantic_search: true,
    hoist_builtin_tools: false,
    bash: false,
    tool_surface: "all",
    disabled_tools: [
      "aft_read",
      "aft_write",
      "aft_edit",
      "aft_apply_patch",
      "grep",
      "glob",
      "aft_safety",
      "aft_import",
      "ast_grep_search",
      "ast_grep_replace",
      "aft_conflicts",
      "lsp_diagnostics",
      "aft_refactor",
      "aft_move",
      "aft_delete",
    ],
  };
}

function codegraphMcpConfig(repoPath: string): Record<string, unknown> {
  return {
    codegraph: {
      type: "local",
      command: ["codegraph", "serve", "--mcp", "--path", repoPath, "--no-watch"],
      enabled: true,
    },
  };
}

/**
 * The built-in OpenCode tool mechanics exposed to BOTH arms. Kept identical so
 * the comparison isolates the code-intelligence layer. Recorded in metadata so
 * a reader can verify fairness without re-deriving it from the config.
 */
function sharedBuiltinTools(): Record<string, string> {
  return {
    read: "native OpenCode read",
    edit: "native OpenCode edit",
    write: "native OpenCode write",
    grep: "native OpenCode grep",
    glob: "native OpenCode glob",
    bash: "native OpenCode bash",
  };
}

/**
 * Resolved, self-describing config for one arm — recorded into report metadata
 * so two runs can be confirmed to use the same setup (Bug #2 fix).
 */
function resolveArmConfig(arm: AgentArm): ArmConfig {
  if (arm === "aft") {
    return {
      arm,
      intelligenceLayer: AFT_PLUGIN_SPEC,
      aft: aftBenchConfig(),
      builtinTools: sharedBuiltinTools(),
      preWarm: "aft warmup --root <repo> (builds lexical + semantic index)",
    };
  }
  return {
    arm,
    intelligenceLayer: CODEGRAPH_PKG,
    // Use a stable placeholder, not the per-task repo path, so the recorded
    // config is comparable across runs and tasks.
    mcp: codegraphMcpConfig("<per-task repo>"),
    builtinTools: sharedBuiltinTools(),
    preWarm: "codegraph init <repo> && codegraph index <repo> --quiet",
  };
}

function codegraphEnv(): Record<string, string> {
  return { CI: "1", CODEGRAPH_NO_WATCH: "1", CODEGRAPH_NO_DAEMON: "1" };
}

function cortexKitDataHome(): string {
  return process.env.XDG_DATA_HOME ?? join(homedir(), ".local", "share");
}

function cortexKitStorageRoot(): string {
  return join(cortexKitDataHome(), "cortexkit", "aft");
}

function nonEmpty(value: string | undefined): string | undefined {
  const trimmed = value?.trim();
  return trimmed ? trimmed : undefined;
}

function isRateLimited(stdout: string, stderr: string, exitCode: number): boolean {
  if (exitCode === 0) return false;
  return /rate.?limit|\b429\b|too many requests/i.test(`${stdout}\n${stderr}`);
}

function extractVisibleText(stdout: string): string {
  const chunks: string[] = [];
  for (const line of stdout.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    try {
      collectText(JSON.parse(trimmed), chunks);
    } catch {
      chunks.push(trimmed);
    }
  }
  return chunks.join("\n").trim();
}

function collectText(value: unknown, chunks: string[]): void {
  if (!value || typeof value !== "object") return;
  if (Array.isArray(value)) {
    for (const item of value) collectText(item, chunks);
    return;
  }
  const obj = value as Record<string, unknown>;
  const type = typeof obj.type === "string" ? obj.type : "";
  if (typeof obj.text === "string" && (type.includes("text") || type.includes("message") || type.includes("part"))) chunks.push(obj.text);
  if (typeof obj.content === "string" && (type.includes("message") || type.includes("part") || type === "assistant")) chunks.push(obj.content);
  for (const nested of Object.values(obj)) collectText(nested, chunks);
}

function parseTokenUsage(stdout: string): TokenUsage {
  let input = 0;
  let output = 0;
  let total = 0;
  for (const line of stdout.split("\n")) {
    try {
      collectTokens(JSON.parse(line), (kind, value) => {
        if (kind === "input") input = Math.max(input, value);
        else if (kind === "output") output = Math.max(output, value);
        else total = Math.max(total, value);
      });
    } catch {}
  }
  if (total === 0) total = input + output;
  return { input, output, total };
}

/**
 * Decompose where a run's tokens go so each result is self-describing for the
 * fixed-overhead (system prompt + tool defs) vs variable (tool output / turns)
 * analysis. Parses the OpenCode `--format json` event stream:
 *   - step_finish parts carry cumulative `tokens` (first step.input ≈ system+tools+task)
 *   - tool_use parts carry `state.output` (re-fed into context next turn)
 *   - text parts carry assistant text
 */
function parseTokenBreakdown(stdout: string): TokenBreakdown {
  let promptInputTokens = 0;
  let toolOutputChars = 0;
  let assistantTextChars = 0;
  let steps = 0;
  let firstInputSeen = false;
  for (const line of stdout.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    let event: Record<string, unknown>;
    try {
      event = JSON.parse(trimmed) as Record<string, unknown>;
    } catch {
      continue;
    }
    const part = (event.part as Record<string, unknown> | undefined) ?? {};
    const partType = typeof part.type === "string" ? part.type : "";
    const tokens = part.tokens as Record<string, unknown> | undefined;
    if (tokens && typeof tokens.input === "number") {
      steps += 1;
      if (!firstInputSeen) {
        promptInputTokens = tokens.input;
        firstInputSeen = true;
      }
    }
    if (event.type === "tool_use" || partType === "tool") {
      const state = part.state as Record<string, unknown> | undefined;
      const output = state?.output;
      if (typeof output === "string") toolOutputChars += output.length;
    }
    if (partType === "text") {
      const text = part.text;
      if (typeof text === "string") assistantTextChars += text.length;
    }
  }
  return {
    promptInputTokens,
    toolOutputChars,
    toolOutputTokensEst: Math.round(toolOutputChars / 4),
    assistantTextChars,
    steps,
  };
}

/**
 * Render a human-readable [TOOL CALL]/[TOOL RESULT]/[ASSISTANT TEXT] transcript
 * from the OpenCode `--format json` event stream (Bug #1 fix — the harness
 * previously left only stale committed transcripts and never regenerated them).
 */
function renderTranscript(stdout: string): string {
  const lines: string[] = [];
  for (const line of stdout.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    let event: Record<string, unknown>;
    try {
      event = JSON.parse(trimmed) as Record<string, unknown>;
    } catch {
      continue;
    }
    const part = (event.part as Record<string, unknown> | undefined) ?? {};
    const partType = typeof part.type === "string" ? part.type : "";
    if (event.type === "tool_use" || partType === "tool") {
      const tool = typeof part.tool === "string" ? part.tool : "unknown";
      const state = (part.state as Record<string, unknown> | undefined) ?? {};
      const input = state.input;
      lines.push("");
      lines.push(`[TOOL CALL] ${tool}  input=${JSON.stringify(input ?? {})}`);
      const output = state.output;
      lines.push("[TOOL RESULT]");
      if (typeof output === "string") {
        lines.push(output.length > 4000 ? `${output.slice(0, 4000)}\n…(truncated, ${output.length} chars total)` : output);
      } else if (output != null) {
        lines.push(JSON.stringify(output));
      }
    } else if (partType === "text") {
      const text = part.text;
      if (typeof text === "string" && text.trim()) {
        lines.push("");
        lines.push("[ASSISTANT TEXT]");
        lines.push(text.trim());
      }
    }
  }
  const body = lines.join("\n").trim();
  return body ? `${body}\n` : "(no tool calls or assistant text captured)\n";
}

function collectTokens(value: unknown, onToken: (kind: "input" | "output" | "total", value: number) => void): void {
  if (!value || typeof value !== "object") return;
  if (Array.isArray(value)) {
    for (const item of value) collectTokens(item, onToken);
    return;
  }
  for (const [key, nested] of Object.entries(value as Record<string, unknown>)) {
    if (typeof nested === "number" && Number.isFinite(nested)) {
      const normalized = key.replace(/[_-]/g, "").toLowerCase();
      if (normalized === "input" || (normalized.includes("input") && normalized.includes("token"))) onToken("input", nested);
      else if (normalized === "output" || (normalized.includes("output") || normalized.includes("completion")) && normalized.includes("token")) onToken("output", nested);
      else if (normalized === "total" || normalized === "tokens" || normalized === "totaltokens") onToken("total", nested);
    }
    collectTokens(nested, onToken);
  }
}

function countToolCalls(stdout: string): number {
  let count = 0;
  for (const line of stdout.split("\n")) {
    try {
      if (objectMentionsToolCall(JSON.parse(line))) count++;
    } catch {}
  }
  return count;
}

function objectMentionsToolCall(value: unknown): boolean {
  if (!value || typeof value !== "object") return false;
  if (Array.isArray(value)) return value.some(objectMentionsToolCall);
  const obj = value as Record<string, unknown>;
  const type = typeof obj.type === "string" ? obj.type.toLowerCase() : "";
  if (type === "tool_use" || type === "tool" || (type.includes("tool") && (type.includes("call") || type.includes("start") || type.includes("execute")))) return true;
  if (typeof obj.tool === "string" || typeof obj.toolName === "string") return true;
  return Object.values(obj).some(objectMentionsToolCall);
}

function dryRunOutput(task: AgentTask): string {
  const values = task.checks.flatMap((check) => check.type === "answer_contains" ? check.values : []);
  return JSON.stringify({ type: "message", role: "assistant", content: values.length ? Object.fromEntries(values.map((value, index) => [`value${index + 1}`, value])) : { ok: true }, usage: { input_tokens: 0, completion_tokens: 0 } }) + "\n";
}

function applyDryRunEdit(task: AgentTask, repoPath: string): void {
  for (const check of task.checks) {
    if (check.type !== "file_contains") continue;
    const path = resolve(repoPath, check.path);
    if (!existsSync(path)) continue;
    let content = readFileSync(path, "utf8");
    if (content.includes(check.value)) continue;
    if (check.value.includes("MAX_CART_ITEMS = 50")) content = content.replace("MAX_CART_ITEMS = 25", "MAX_CART_ITEMS = 50");
    else if (check.value.includes("FREE_SHIPPING_THRESHOLD = 10000")) content = content.replace("FREE_SHIPPING_THRESHOLD = 7500", "FREE_SHIPPING_THRESHOLD = 10000");
    else if (check.value.includes("SALES_TAX_RATE = 0.075")) content = content.replace("SALES_TAX_RATE = 0.08", "SALES_TAX_RATE = 0.075");
    else if (check.value.includes('status: "queued" | "paid";')) content = content.replace('status: "pending" | "paid";', 'status: "queued" | "paid";');
    else if (check.value.includes("{ attempts: 4, delayMs: 25 }")) content = content.replace("{ attempts: 3, delayMs: 25 }", "{ attempts: 4, delayMs: 25 }");
    writeFileSync(path, content);
  }
}

function emptySummary(): AgentReport["summary"][AgentArm] {
  return { runs: 0, successRate: 0, successes: 0, tokensTotal: 0, tokensMedian: 0, wallTimeMsMedian: 0, wallTimeMsP95: 0, toolCallsMedian: 0 };
}

function truncate(value: string, max: number): string {
  return value.length <= max ? value : `${value.slice(0, max)}…`;
}

function positiveInt(raw: string, name: string): number {
  const value = Number.parseInt(raw, 10);
  if (!Number.isInteger(value) || value <= 0) throw new Error(`${name} must be a positive integer`);
  return value;
}

main().catch((err) => {
  console.error(err instanceof Error ? err.stack ?? err.message : err);
  process.exit(1);
});
