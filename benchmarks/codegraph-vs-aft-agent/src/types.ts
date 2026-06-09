export type AgentArm = "aft" | "codegraph";
export type AgentTaskKind = "answer" | "edit";

export interface AnswerContainsCheck {
  type: "answer_contains";
  values: string[];
}

export interface FileContainsCheck {
  type: "file_contains";
  path: string;
  value: string;
}

export interface FileNotContainsCheck {
  type: "file_not_contains";
  path: string;
  value: string;
}

export interface CommandCheck {
  type: "command";
  command: string;
}

export type AgentCheck = AnswerContainsCheck | FileContainsCheck | FileNotContainsCheck | CommandCheck;

export interface AgentTask {
  id: string;
  kind: AgentTaskKind;
  prompt: string;
  checks: AgentCheck[];
  timeoutMs?: number;
}

export interface AgentCorpus {
  name: string;
  description?: string;
  fixturePath: string;
  tasks: AgentTask[];
}

export interface TokenUsage {
  input: number;
  output: number;
  total: number;
}

/**
 * Per-task decomposition of where tokens go, so every run is self-describing
 * for fixed-overhead (system prompt + tool defs) vs variable (tool output / turns)
 * analysis without re-parsing raw transcripts.
 */
export interface TokenBreakdown {
  /** Input tokens on the FIRST model step ≈ system prompt + tool definitions + task prompt. */
  promptInputTokens: number;
  /** Sum of characters across all tool-call OUTPUTs fed back into context. */
  toolOutputChars: number;
  /** Rough token estimate of tool output (chars / 4). */
  toolOutputTokensEst: number;
  /** Sum of characters of assistant text parts emitted. */
  assistantTextChars: number;
  /** Number of distinct model steps (turns) observed. */
  steps: number;
}

/**
 * The resolved, self-describing configuration for one arm. Recorded into the
 * report metadata so two runs can be confirmed to use the same setup (model,
 * tool surface, disabled tools, MCP/plugin wiring, pre-warm).
 */
export interface ArmConfig {
  arm: AgentArm;
  /** AFT plugin spec or CodeGraph npm package under test. */
  intelligenceLayer: string;
  /** Resolved AFT aft.jsonc config (aft arm only). */
  aft?: Record<string, unknown>;
  /** Resolved CodeGraph MCP wiring (codegraph arm only). */
  mcp?: Record<string, unknown>;
  /** Which built-in OpenCode tool mechanics are exposed (kept identical across arms for fairness). */
  builtinTools: Record<string, string>;
  /** Pre-warm step run before the agent (index build), for parity. */
  preWarm: string;
}

export interface AgentRunResult {
  arm: AgentArm;
  taskId: string;
  kind: AgentTaskKind;
  success: boolean;
  model: string;
  attemptedModel: string;
  wallTimeMs: number;
  exitCode: number;
  tokens: TokenUsage;
  tokenBreakdown: TokenBreakdown;
  toolCalls: number;
  answerText: string;
  stdoutPath: string;
  stderrPath: string;
  transcriptPath: string;
  repoPath: string;
  checks: Array<{ check: AgentCheck; pass: boolean; detail?: string }>;
  error?: string;
}

export interface AgentReport {
  benchmark: "codegraph-vs-aft-agent";
  timestamp: string;
  model: string;
  fallbackModel: string;
  corpus: string;
  arms: AgentArm[];
  taskCount: number;
  summary: Record<AgentArm, {
    runs: number;
    successRate: number;
    successes: number;
    tokensTotal: number;
    tokensMedian: number;
    wallTimeMsMedian: number;
    wallTimeMsP95: number;
    toolCallsMedian: number;
  }>;
  results: AgentRunResult[];
  armConfigs: Record<AgentArm, ArmConfig>;
  metadata: Record<string, unknown>;
}
