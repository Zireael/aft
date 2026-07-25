import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import {
  type ConfigureExperimentalOverrides,
  type ConfigureLspOverrides,
  type ConfigureLspServer,
  DEFAULT_BRIDGE_HANG_THRESHOLD,
  DEFAULT_BRIDGE_REQUEST_TIMEOUT_MS,
  detectConfigFile,
  FOREGROUND_WAIT_WINDOW_DEFAULT_MS,
  FOREGROUND_WAIT_WINDOW_MIN_MS,
  getProjectLspStrippedKeys,
  mergeBashConfig,
  mergeExperimentalConfig,
  mergeInspectConfig,
  mergeLspConfig,
  mergeSemanticConfig,
  migrateAftConfigFile,
  migrateRawConfig,
  type ResolvedBashConfig,
  resolveBashConfig,
  resolveExperimentalConfigForConfigure,
  resolveLspConfigForConfigure,
  stripJsoncSymbols,
} from "@cortexkit/aft-bridge";
import { parse as parseJsonc } from "comment-json";
import { z } from "zod";
import { error, log, warn } from "./logger.js";

export {
  type ConfigureExperimentalOverrides,
  type ConfigureLspOverrides,
  type ConfigureLspServer,
  DEFAULT_BRIDGE_HANG_THRESHOLD,
  DEFAULT_BRIDGE_REQUEST_TIMEOUT_MS,
  FOREGROUND_WAIT_WINDOW_DEFAULT_MS,
  FOREGROUND_WAIT_WINDOW_MIN_MS,
  migrateAftConfigFile,
  type ResolvedBashConfig,
  resolveBashConfig,
  resolveExperimentalConfigForConfigure,
  resolveLspConfigForConfigure,
};

// ---------------------------------------------------------------------------
// Zod schema
// ---------------------------------------------------------------------------

const FormatterEnum = z.enum([
  "biome",
  "oxfmt",
  "prettier",
  "deno",
  "ruff",
  "black",
  "rustfmt",
  "goimports",
  "gofmt",
  "none",
]);

const CheckerEnum = z.enum([
  "tsc",
  "tsgo",
  "biome",
  "pyright",
  "ruff",
  "cargo",
  "go",
  "staticcheck",
  "none",
]);

/** How configure-time missing-tool warnings reach the user. Default: toast (no chat transcript). */
export const ConfigureWarningsDeliveryEnum = z.enum(["toast", "log", "chat"]);
export type ConfigureWarningsDelivery = z.infer<typeof ConfigureWarningsDeliveryEnum>;

const SemanticBackendEnum = z.enum([
  "fastembed",
  "openai_compatible",
  "ollama",
  "model2vec",
  "perplexity",
]);

const SemanticConfigSchema = z.object({
  /** Semantic backend type: local fastembed, OpenAI-compatible API, Ollama, model2vec, or Perplexity. */
  backend: SemanticBackendEnum.optional(),
  /** Model identifier passed to the selected semantic backend. */
  model: z.string().trim().min(1).optional(),
  /** Base URL of the backend API endpoint. */
  base_url: z.string().trim().min(1).optional(),
  /** Environment variable that contains the API key used by external backends. */
  api_key_env: z.string().trim().min(1).optional(),
  /** Backend request timeout in milliseconds. */
  timeout_ms: z.number().int().positive().optional(),
  /** Maximum batch size used by the semantic pipeline. */
  max_batch_size: z.number().int().positive().optional(),
  /** Maximum number of project files to semantically index (default 20000). */
  max_files: z.number().int().positive().optional(),
  /** Output encoding used by the embedding backend (e.g. base64_binary). */
  output_encoding: z.enum(["float32", "base64_binary", "binary_packed"]).optional(),
  /** Storage strategy for cached embeddings (e.g. binary_packed). */
  storage_strategy: z.enum(["float32", "binary_packed", "sqlite"]).optional(),
  /** Input mode for the embedding backend (e.g. document_chunks). */
  input_mode: z.enum(["document_chunks", "symbol_chunks", "full_file"]).optional(),
  /** Embedding dimensions (overrides model default). */
  dimensions: z.number().int().positive().optional(),
  /** Distance metric for similarity search (e.g. cosine, dot_product, euclidean). */
  distance_metric: z.enum(["cosine", "dot_product", "euclidean"]).optional(),
  /** Local path to a model2vec model directory (user-only trust boundary). */
  model_path: z.string().trim().min(1).optional(),
  /** Maximum sequence length for model2vec tokenization. */
  model2vec_max_length: z.number().int().positive().optional(),
  /** Enable optional reranking via an OpenAI-compatible endpoint (default: false). */
  rerank_enabled: z.boolean().optional(),
  /** Override model for reranking. Defaults to codellama/codellama:7b-instruct if unset. */
  rerank_model: z.string().optional(),
  /** Base URL for reranker endpoint. Falls back to base_url if unset. */
  rerank_base_url: z.string().optional(),
  /** Env var name for reranker API key. Falls back to api_key_env if unset. */
  rerank_api_key_env: z.string().optional(),
  /** Timeout in ms for reranker requests (default: 15000). */
  rerank_timeout_ms: z.number().optional(),
  /** Max number of candidates to send to the reranker per query (default: 20). */
  rerank_max_candidates: z.number().optional(),
  /** Max characters per candidate snippet sent to reranker (default: 2500). */
  rerank_max_candidate_chars: z.number().optional(),
  /** Reranker API format: "chat" for LLM-based (default), "rerank" for cross-encoder. */
  rerank_api_type: z.enum(["chat", "rerank"]).optional(),
  /** Max chars per candidate for cross-encoder rerankers (default: 512). */
  rerank_max_candidate_chars_cross_encoder: z.number().optional(),
  /** Optional override for the reranker prompt template. Use {query} and {candidates}. */
  rerank_prompt_template: z.string().optional(),
  /** Enable per-query search diagnostics collection (default: false). */
  diagnostics_enabled: z.boolean().optional(),
  /** How much diagnostic detail to include in tool output: "off", "minimal", "verbose". */
  output_mode: z.enum(["off", "minimal", "verbose"]).optional(),
  /** Optional template applied to user queries before embedding. Use {query} placeholder. */
  query_prompt_template: z.string().optional(),
  /** Optional template applied to document/chunk text before embedding. Use {text} placeholder. */
  document_prompt_template: z.string().optional(),
  /** Auto-detect embedding model and apply built-in prefixes (default: true). */
  use_model_profiles: z.boolean().optional(),
  /** Maximum results returned per file after hybrid fusion (default: 2). */
  max_results_per_file: z.number().optional(),
  /** Max tokens per embedding request for remote backends (default: 512). */
  max_embed_tokens: z.number().optional(),
  /** Overlapping tokens between chunks when splitting large symbols (default: 100). */
  chunk_overlap_tokens: z.number().optional(),
});

const LspExtensionSchema = z
  .string()
  .trim()
  .min(1)
  .refine((value) => value.replace(/^\.+/, "").length > 0, {
    message: "Extension must include characters other than leading dots",
  });

const LspServerEntrySchema = z.object({
  // Optional: when overriding a built-in server (e.g. `rust`) to tweak one
  // field, AFT inherits the built-in's extensions/binary. Requiring them here
  // silently dropped the whole `lsp` section on a partial override.
  extensions: z.array(LspExtensionSchema).min(1).optional(),
  binary: z.string().trim().min(1).optional(),
  args: z.array(z.string()).optional().default([]),
  root_markers: z.array(z.string().trim().min(1)).optional().default([".git"]),
  disabled: z.boolean().optional().default(false),
  /** Extra environment variables passed to the LSP server child process. */
  env: z.record(z.string().min(1), z.string()).optional(),
  /** JSON value passed as `initializationOptions` in the LSP `initialize` request. */
  initialization_options: z.unknown().optional(),
});

export const LspServerSchema = LspServerEntrySchema.extend({
  id: z.string().trim().min(1),
});

const LspConfigSchema = z.object({
  servers: z.record(z.string().trim().min(1), LspServerEntrySchema).optional(),
  disabled: z.array(z.string().trim().min(1)).optional(),
  python: z.enum(["pyright", "ty", "auto"]).optional(),
  /**
   * Restore legacy edit behavior by waiting for inline LSP diagnostics on every
   * edit/write/apply_patch call unless the tool call overrides diagnostics.
   * Default: false.
   */
  diagnostics_on_edit: z.boolean().optional(),
  /**
   * Auto-install npm-distributed and GitHub-release language servers when
   * the project needs them. Default: true. Set false to require manual
   * install via PATH.
   */
  auto_install: z.boolean().optional(),
  /**
   * Supply-chain grace window. AFT only installs versions that have been
   * on the registry / GitHub releases for at least this many days, defending
   * against newly-published malicious versions that get yanked within hours
   * of detection. Default: 7. User pins via `lsp.versions` bypass this.
   */
  // Audit-2 v0.17 #10: grace_days must be >= 1 because grace_days: 0 disables
  // the supply-chain grace window entirely with no warning. Users debugging
  // can still bypass the grace per-package via `lsp.versions` pins, which is
  // a more explicit and auditable opt-out.
  grace_days: z.number().int().positive().optional(),
  /**
   * Per-package version pin map keyed by npm package or GitHub repo. Pins
   * bypass the grace filter and any weekly version recheck. Examples:
   *   { "typescript-language-server": "5.0.0" }
   *   { "clangd/clangd": "21.1.0" }
   */
  versions: z.record(z.string().trim().min(1), z.string().trim().min(1)).optional(),
});

const ExperimentalConfigSchema = z.object({
  /**
   * @deprecated The bash family graduated from experimental in v0.27.2. Use the
   * top-level `bash` key instead. This nested form is still accepted for
   * backward compatibility — when present and top-level `bash` is absent,
   * its values seed the resolved bash config. Will be removed in v0.28.
   */
  bash: z
    .object({
      rewrite: z.boolean().optional(),
      compress: z.boolean().optional(),
      background: z.boolean().optional(),
      long_running_reminder_enabled: z.boolean().optional(),
      long_running_reminder_interval_ms: z.number().int().positive().optional(),
    })
    .optional(),
  lsp_ty: z.boolean().optional(),
});

/**
 * Graduated `bash` config. Replaces `experimental.bash.*` in v0.27.2.
 * Default behavior:
 *   - tool_surface "recommended" or "all" → bash hoist on, all sub-features on
 *   - tool_surface "minimal" → bash hoist off (user explicitly wants minimal)
 * Three shapes:
 *   - `bash: true`     → identical to default (all on)
 *   - `bash: false`    → hoist disabled entirely; OpenCode native bash stays
 *   - `bash: { ... }`  → partial override; missing sub-keys default to true
 */
const BashFeaturesSchema = z.object({
  rewrite: z.boolean().optional(),
  compress: z.boolean().optional(),
  background: z.boolean().optional(),
  /**
   * Allow OpenCode subagents to use real background bash (`background: true`
   * and auto-promotion). Default: false — subagents fall back to synchronous
   * foreground polling because they can't survive turn-end to receive the
   * wake-up reminder. When true, subagents get the same bg semantics as
   * primary sessions and MUST explicitly wait for their bg tasks with
   * `bash_status({ taskId, exit: true, ... })` before returning to parent.
   * Setting this is essentially a contract with your subagent prompts that
   * they know how to use bash_status's wait mode.
   */
  subagent_background: z.boolean().optional(),
  long_running_reminder_enabled: z.boolean().optional(),
  long_running_reminder_interval_ms: z.number().int().positive().optional(),
  /**
   * How long foreground bash blocks before auto-promoting the task to
   * background. Default 8000ms; values below the 5000ms floor are clamped up.
   */
  foreground_wait_window_ms: z.number().int().positive().optional(),
});

const BashConfigSchema = z.union([z.boolean(), BashFeaturesSchema]);

const BridgeConfigSchema = z.object({
  /**
   * Per-request bridge transport timeout in milliseconds. Default: 30000.
   * Raise on slow filesystems (WSL/DrvFs/NFS) where cold `aft` operations exceed the default.
   */
  request_timeout_ms: z
    .number()
    .int()
    .min(1000, { message: "bridge.request_timeout_ms must be at least 1000" })
    .optional(),
  /**
   * Consecutive silent request timeouts before the bridge is killed and respawned.
   * Default: 2. Raise when many editor windows share one bridge process.
   */
  hang_threshold: z
    .number()
    .int()
    .min(1, { message: "bridge.hang_threshold must be at least 1" })
    .optional(),
});

const InspectConfigSchema = z.object({
  /** Master switch for the aft_inspect tool. Defaults to true. */
  enabled: z.boolean().optional(),
  /** OpenCode session.idle delay before Tier 2 inspect prewarm. Default: 4 minutes. */
  tier2_idle_minutes: z.number().min(0).optional(),
  categories: z.record(z.string(), z.boolean()).optional(),
  tier2_soft_deadline_ms: z.number().int().positive().optional(),
  max_drill_down_items: z.number().int().positive().max(100).optional(),
  duplicates: z
    .object({
      lower_bound: z.number().int().positive().optional(),
      discard_cost: z.number().int().min(0).optional(),
      anonymize: z
        .object({
          variables: z.boolean().optional(),
          fields: z.boolean().optional(),
          methods: z.boolean().optional(),
          types: z.boolean().optional(),
          literals: z.boolean().optional(),
        })
        .optional(),
    })
    .optional(),
});

export const AftConfigSchema = z
  .object({
    /**
     * Optional JSON Schema URL for editor tooling. Ignored by the plugin at
     * runtime — only present so VS Code/Cursor/etc. pick up the published
     * schema for autocomplete + validation. `aft setup` auto-inserts this.
     */
    $schema: z.string().optional(),
    /** Whether to auto-format files after edits. Default: true. */
    format_on_edit: z.boolean().optional(),
    /**
     * Maximum seconds an external formatter is allowed to run before AFT
     * kills it and reports `format_skipped_reason: "timeout"`. Bounded
     * 1..=600. Default: 10. Raise for slow formatters (e.g. ruff in large
     * Python projects); lower for tighter test loops.
     */
    formatter_timeout_secs: z.number().int().min(1).max(600).optional(),
    /** Auto-validate after edits: "syntax" (tree-sitter) or "full" (runs type checker). */
    validate_on_edit: z.enum(["syntax", "full"]).optional(),
    /** Per-language formatter overrides. Keys: "typescript", "python", "rust", "go". */
    formatter: z.record(z.string(), FormatterEnum).optional(),
    /** Per-language type checker overrides. Keys: "typescript", "python", "rust", "go". */
    checker: z.record(z.string(), CheckerEnum).optional(),
    /**
     * How missing formatter/checker/LSP warnings are shown after configure.
     * - `toast`: 10s TUI toast (or HTTP show-toast when available); no session chat
     * - `log`: plugin log only
     * - `chat`: legacy ignored user messages in the session transcript
     *
     * There is no top-level `formatters` key — use `format_on_edit`, `formatter`, and
     * `checker` instead.
     */
    configure_warnings_delivery: ConfigureWarningsDeliveryEnum.optional(),
    /**
     * Replace opencode's built-in read/write/edit/apply_patch tools with AFT's
     * faster Rust implementations. Adds backup tracking, auto-formatting,
     * inline diagnostics, and permission checks. Default: true.
     */
    hoist_builtin_tools: z.boolean().optional(),
    /**
     * Tool surface level. Controls which tools are registered:
     * - "minimal":     aft_outline, aft_zoom, aft_safety (no hoisting)
     * - "recommended": minimal + hoisted read/write/edit/apply_patch
     *                  + ast_grep_search/replace + aft_import (default)
     * - "all":         recommended + aft_callgraph, aft_delete, aft_move, aft_refactor
     */
    tool_surface: z.enum(["minimal", "recommended", "all"]).optional(),
    /**
     * List of tool names to disable. Disabled tools are not registered with
     * OpenCode and will be invisible to agents. Use exact tool names, e.g.
     * ["aft_callgraph", "aft_refactor"]. Hoisted names ("read", "edit") and
     * aft-prefixed names both work. Applied after tool_surface filtering.
     */
    disabled_tools: z.array(z.string()).optional(),
    /**
     * Restrict file operations to within the project root directory.
     * When true, write-capable commands reject paths outside project_root.
     * Default: false (matches OpenCode's built-in behavior).
     */
    restrict_to_project_root: z.boolean().optional(),
    /** Enable indexed search for grep and glob hoisting. Default: false. */
    search_index: z.boolean().optional(),
    /** Enable semantic search. Default: false. */
    semantic_search: z.boolean().optional(),
    /** FTS5 full-text search configuration. Default: { enabled: false }. */
    fts5: z
      .object({
        enabled: z.boolean().optional(),
        auto_index: z.boolean().optional(),
        index_on_start: z.boolean().optional(),
        max_results: z.number().optional(),
        /** Maximum characters stored per symbol body (default: 2000). */
        max_body_chars: z.number().optional(),
        /** Maximum lines stored per symbol body (default: 60). */
        max_body_lines: z.number().optional(),
        /** Enable raw FTS5 debug output in search results (default: false). */
        raw_fts_debug: z.boolean().optional(),
      })
      .optional(),
    /** Codebase health inspection config. Enabled by default; set inspect.enabled=false to hide aft_inspect. */
    inspect: InspectConfigSchema.optional(),
    /**
     * Bash tool family (hoist + rewrite + compress + background execution).
     * Default on for `tool_surface: recommended`/`all`, off for `minimal`.
     *
     * Accepts three shapes:
     *   - `true`  — all sub-features on, hoist enabled
     *   - `false` — hoist disabled entirely; OpenCode's native bash stays
     *   - `{ rewrite?, compress?, background?, ... }` — partial override;
     *     missing sub-keys default to `true`
     *
     * Replaces `experimental.bash.*` (still accepted for backward compat).
     */
    bash: BashConfigSchema.optional(),
    /** Experimental opt-in features. Default: all false. */
    experimental: ExperimentalConfigSchema.optional(),
    /** User-defined and built-in LSP server configuration. */
    lsp: LspConfigSchema.optional(),
    /** Allow URL fetch tools to request private/link-local hosts. Default: false. */
    url_fetch_allow_private: z.boolean().optional(),
    /** External semantic backend configuration for embedding and retrieval. */
    semantic: SemanticConfigSchema.optional(),
    /**
     * Maximum source files allowed for call-graph operations (callers, trace_to,
     * trace_to_symbol, trace_data, impact). Projects above this size return `project_too_large`
     * instead of attempting the reverse-index build. Does not affect grep,
     * glob, read, edit, or any other tool. Default: 5000.
     */
    max_callgraph_files: z.number().int().positive().optional(),
    /** Auto-refresh OpenCode's cached @cortexkit/aft-opencode package when a newer channel version exists. */
    auto_update: z.boolean().optional(),
    /** Per-bridge transport timeout and hang-escalation (USER-only; shared pool). */
    bridge: BridgeConfigSchema.optional(),
  })
  .strict();

export type AftConfig = z.infer<typeof AftConfigSchema>;

export type LspServerConfig = z.infer<typeof LspServerSchema>;

/**
 * Build the per-project subset of configure overrides that come from
 * `aft.jsonc` (user config merged with project config). Used by the OpenCode
 * plugin's per-bridge `projectConfigLoader` so each project's `aft.jsonc` wins
 * over the user-level config for that project's bridge, instead of every
 * bridge inheriting whatever project was visible at plugin init.
 *
 * **DO NOT** put genuinely-global fields here. Things like `storage_dir`,
 * `_ort_dylib_dir`, `harness`, `lsp_paths_extra`, `bash_permissions` are set
 * at plugin init from process state (XDG dirs, ONNX download path, etc.) and
 * MUST NOT be re-derived per-bridge — they're identical across all bridges in
 * one OpenCode/Pi process.
 *
 * **DO NOT** put fields that affect plugin-side tool registration here.
 * `tool_surface`, `disabled_tools`, and `hoist_builtin_tools` lock at plugin
 * init because OpenCode registers tools synchronously when the plugin
 * function returns. Per-bridge changes to those fields wouldn't take effect.
 */
export function resolveProjectOverridesForConfigure(config: AftConfig): Record<string, unknown> {
  const overrides: Record<string, unknown> = {};

  // Edit-pipeline behavior — overridable per-project.
  if (config.format_on_edit !== undefined) overrides.format_on_edit = config.format_on_edit;
  if (config.formatter_timeout_secs !== undefined)
    overrides.formatter_timeout_secs = config.formatter_timeout_secs;
  if (config.validate_on_edit !== undefined) overrides.validate_on_edit = config.validate_on_edit;
  if (config.formatter !== undefined) overrides.formatter = config.formatter;
  if (config.checker !== undefined) overrides.checker = config.checker;

  // Project containment — default false at the plugin layer (parity with
  // OpenCode's built-in tools). Users opt in with `restrict_to_project_root: true`.
  overrides.restrict_to_project_root = config.restrict_to_project_root ?? false;

  // Indexed search and semantic search — both are per-project opt-ins.
  if (config.search_index !== undefined) overrides.search_index = config.search_index;
  if (config.semantic_search !== undefined) overrides.semantic_search = config.semantic_search;
  if (config.fts5 !== undefined) overrides.fts5 = config.fts5;

  // Bash / LSP / semantic / max_callgraph_files — all flow through dedicated
  // resolvers because they have their own merge / project-safety rules.
  Object.assign(overrides, resolveExperimentalConfigForConfigure(config, true));
  Object.assign(overrides, resolveLspConfigForConfigure(config));
  if (config.semantic !== undefined) overrides.semantic = config.semantic;
  if (config.inspect !== undefined) overrides.inspect = config.inspect;
  if (config.max_callgraph_files !== undefined)
    overrides.max_callgraph_files = config.max_callgraph_files;

  return overrides;
}

// ---------------------------------------------------------------------------
// Partial parse (valid sections survive, invalid sections are skipped)
// ---------------------------------------------------------------------------

function parseConfigPartially(rawConfig: Record<string, unknown>): AftConfig | null {
  const fullResult = AftConfigSchema.safeParse(rawConfig);
  if (fullResult.success) {
    return fullResult.data;
  }

  const partialConfig: Record<string, unknown> = {};
  const invalidSections: string[] = [];

  for (const key of Object.keys(rawConfig)) {
    const sectionResult = AftConfigSchema.safeParse({ [key]: rawConfig[key] });
    if (sectionResult.success) {
      const parsed = sectionResult.data as Record<string, unknown>;
      if (parsed[key] !== undefined) {
        partialConfig[key] = parsed[key];
      }
    } else {
      const sectionErrors = sectionResult.error.issues
        .filter((i) => i.path[0] === key)
        .map((i) => `${i.path.join(".")}: ${i.message}`)
        .join(", ");
      if (sectionErrors) {
        invalidSections.push(`${key}: ${sectionErrors}`);
      }
    }
  }

  if (invalidSections.length > 0) {
    warn(`Partial config loaded — invalid sections skipped: ${invalidSections.join("; ")}`);
  }

  return partialConfig as AftConfig;
}

// ---------------------------------------------------------------------------
// Load config from a single file path
// ---------------------------------------------------------------------------

function loadConfigFromPath(configPath: string): AftConfig | null {
  try {
    if (!existsSync(configPath)) {
      return null;
    }

    const content = readFileSync(configPath, "utf-8");
    const rawConfig = parseJsonc<Record<string, unknown>>(content);
    migrateRawConfig(rawConfig, configPath, { log, warn });
    // comment-json attaches Symbol(before/after:<key>) props to track comments.
    // Zod stringifies keys when building error paths, which throws on those
    // symbols and would silently drop the whole config to defaults (issue #88).
    // Validate against a symbol-free deep copy; the migration disk-write path
    // above still uses the symbol-bearing object so comments survive.
    const cleanConfig = stripJsoncSymbols(rawConfig);
    const result = AftConfigSchema.safeParse(cleanConfig);

    if (result.success) {
      log(`Config loaded from ${configPath}`);
      return result.data;
    }

    const errorMsg = result.error.issues.map((i) => `${i.path.join(".")}: ${i.message}`).join(", ");
    warn(`Config validation error in ${configPath}: ${errorMsg}`);

    return parseConfigPartially(cleanConfig);
  } catch (err) {
    const errorMsg = err instanceof Error ? err.message : String(err);
    error(`Error loading config from ${configPath}: ${errorMsg}`);
    return null;
  }
}

/**
 * Top-level fields that are SAFE to inherit from project config.
 *
 * Anything NOT in this list flows from user config only. This is the
 * strict-allowlist trust boundary.
 */
const PROJECT_SAFE_TOP_LEVEL_FIELDS = new Set<keyof AftConfig>([
  "tool_surface",
  "hoist_builtin_tools",
  "format_on_edit",
  "validate_on_edit",
  "configure_warnings_delivery",
  "search_index",
  "semantic_search",
  "fts5",
  "inspect",
  "experimental",
  "bash",
]);

function pickProjectSafeFields(override: AftConfig): Partial<AftConfig> {
  const safe: Partial<AftConfig> = {};
  for (const key of PROJECT_SAFE_TOP_LEVEL_FIELDS) {
    if (override[key] !== undefined) {
      // biome-ignore lint/suspicious/noExplicitAny: field-by-field copy with key set guarantee
      (safe as any)[key] = override[key];
    }
  }
  return safe;
}

function getStrippedTopLevelKeys(override: AftConfig): string[] {
  const stripped: string[] = [];
  if (override.restrict_to_project_root !== undefined) stripped.push("restrict_to_project_root");
  if (override.url_fetch_allow_private !== undefined) stripped.push("url_fetch_allow_private");
  if (override.max_callgraph_files !== undefined) stripped.push("max_callgraph_files");
  if (override.auto_update !== undefined) stripped.push("auto_update");
  if (override.bridge !== undefined) stripped.push("bridge");
  return stripped;
}

function mergeConfigs(base: AftConfig, override: AftConfig): AftConfig {
  const disabledTools = [...(base.disabled_tools ?? []), ...(override.disabled_tools ?? [])];
  const formatter = { ...base.formatter, ...override.formatter };
  const checker = { ...base.checker, ...override.checker };
  const semantic = mergeSemanticConfig(base.semantic, override.semantic) as AftConfig["semantic"];
  const lsp = mergeLspConfig(base.lsp, override.lsp) as AftConfig["lsp"];
  const experimental = mergeExperimentalConfig(
    base.experimental,
    override.experimental,
  ) as AftConfig["experimental"];
  const bash = mergeBashConfig(base.bash, override.bash) as AftConfig["bash"];
  const inspect = mergeInspectConfig(base.inspect, override.inspect) as AftConfig["inspect"];
  const bridge = base.bridge;

  const safeOverride = pickProjectSafeFields(override);
  delete safeOverride.bash;
  delete safeOverride.inspect;

  return {
    ...base,
    ...safeOverride,
    ...(Object.keys(formatter).length > 0 ? { formatter } : {}),
    ...(Object.keys(checker).length > 0 ? { checker } : {}),
    ...(lsp ? { lsp } : {}),
    ...(bash !== undefined ? { bash } : {}),
    ...(inspect !== undefined ? { inspect } : {}),
    experimental,
    semantic,
    ...(bridge !== undefined ? { bridge } : {}),
    ...(disabledTools.length > 0 ? { disabled_tools: [...new Set(disabledTools)] } : {}),
  };
}

export function resolveBridgePoolTransportOptions(config: AftConfig): {
  timeoutMs: number;
  hangThreshold: number;
} {
  return {
    timeoutMs: config.bridge?.request_timeout_ms ?? DEFAULT_BRIDGE_REQUEST_TIMEOUT_MS,
    hangThreshold: config.bridge?.hang_threshold ?? DEFAULT_BRIDGE_HANG_THRESHOLD,
  };
}

// ---------------------------------------------------------------------------
// OpenCode config directory detection (same logic as oh-my-opencode)
// ---------------------------------------------------------------------------

function getOpenCodeConfigDir(): string {
  const envDir = process.env.OPENCODE_CONFIG_DIR?.trim();
  if (envDir) {
    return envDir;
  }

  // XDG_CONFIG_HOME or homedir()/.config, then /opencode
  const xdgConfig = process.env.XDG_CONFIG_HOME || join(homedir(), ".config");
  return join(xdgConfig, "opencode");
}

// ---------------------------------------------------------------------------
// Public API: loadAftConfig
// ---------------------------------------------------------------------------

/**
 * Load AFT config using the same two-level pattern as oh-my-opencode:
 *
 * 1. User-level:    ~/.config/opencode/aft.jsonc (or .json)
 * 2. Project-level: <project>/.opencode/aft.jsonc (or .json)
 *
 * Project config merges on top of user config.
 * Both support JSONC (comments allowed).
 * Invalid sections are skipped, valid sections still load.
 */
export function loadAftConfig(projectDirectory: string): AftConfig {
  // User-level config
  const configDir = getOpenCodeConfigDir();
  const userBasePath = join(configDir, "aft");
  migrateAftConfigFile(`${userBasePath}.jsonc`);
  migrateAftConfigFile(`${userBasePath}.json`);
  const userDetected = detectConfigFile(userBasePath);
  const userConfigPath =
    userDetected.format !== "none" ? userDetected.path : `${userBasePath}.json`;

  // Project-level config
  const projectBasePath = join(projectDirectory, ".opencode", "aft");
  migrateAftConfigFile(`${projectBasePath}.jsonc`);
  migrateAftConfigFile(`${projectBasePath}.json`);
  const projectDetected = detectConfigFile(projectBasePath);
  const projectConfigPath =
    projectDetected.format !== "none" ? projectDetected.path : `${projectBasePath}.json`;

  // Load user config first (base)
  let config: AftConfig = loadConfigFromPath(userConfigPath) ?? {};

  // Override with project config
  const projectConfig = loadConfigFromPath(projectConfigPath);
  if (projectConfig) {
    const sensitiveSemanticKeys: string[] = [];
    if (projectConfig.semantic?.backend !== undefined) sensitiveSemanticKeys.push("backend");
    if (projectConfig.semantic?.base_url !== undefined) sensitiveSemanticKeys.push("base_url");
    if (projectConfig.semantic?.api_key_env !== undefined)
      sensitiveSemanticKeys.push("api_key_env");
    if (sensitiveSemanticKeys.length > 0) {
      warn(
        "Ignoring semantic.backend, base_url, api_key_env from project config (security: these semantic settings only honor user-level config)",
      );
    }

    const newSemanticFields = [
      "output_encoding",
      "storage_strategy",
      "input_mode",
      "dimensions",
      "distance_metric",
    ];
    const strippedNewSemanticFields = newSemanticFields.filter(
      (field) =>
        projectConfig.semantic?.[field as keyof typeof projectConfig.semantic] !== undefined,
    );
    if (strippedNewSemanticFields.length > 0) {
      warn(
        `Ignoring semantic.${strippedNewSemanticFields.join(", ")} from project config (security: trust-boundary fields — use user config for semantic backend tuning)`,
      );
    }

    const model2vecSemanticFields = ["model_path", "model2vec_max_length"];
    const strippedModel2vecFields = model2vecSemanticFields.filter(
      (field) =>
        projectConfig.semantic?.[field as keyof typeof projectConfig.semantic] !== undefined,
    );
    if (strippedModel2vecFields.length > 0) {
      warn(
        `Ignoring semantic.${strippedModel2vecFields.join(", ")} from project config (security: trust-boundary fields — use user config for model2vec settings)`,
      );
    }

    if (
      projectConfig.semantic?.rerank_prompt_template !== undefined ||
      projectConfig.semantic?.query_prompt_template !== undefined ||
      projectConfig.semantic?.document_prompt_template !== undefined
    ) {
      warn(
        "Ignoring semantic.rerank_prompt_template/query_prompt_template/document_prompt_template from project config (security: trust-boundary fields — use user config for prompt templates)",
      );
    }
    const strippedLspKeys = getProjectLspStrippedKeys(projectConfig.lsp);
    if (strippedLspKeys.length > 0) {
      warn(
        `Ignoring ${strippedLspKeys.join(", ")} from project config ${projectConfigPath} (security: these LSP settings only honor user-level config)`,
      );
    }
    const strippedTopLevelKeys = getStrippedTopLevelKeys(projectConfig);
    if (strippedTopLevelKeys.length > 0) {
      warn(
        `Ignoring ${strippedTopLevelKeys.join(", ")} from project config ${projectConfigPath} (security: these settings only honor user-level config — a project should not weaken security boundaries for the user)`,
      );
    }
    config = mergeConfigs(config, projectConfig);
  }

  return config;
}
