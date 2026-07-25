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
// Zod schema (single source of truth; types derived via z.infer<>)
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

const ConfigureWarningsDeliveryEnum = z.enum(["toast", "log", "chat"]);

const SemanticConfigSchema = z.object({
  backend: z
    .enum(["fastembed", "openai_compatible", "ollama", "model2vec", "perplexity"])
    .optional(),
  model: z.string().trim().min(1).optional(),
  base_url: z.string().trim().min(1).optional(),
  api_key_env: z.string().trim().min(1).optional(),
  timeout_ms: z.number().int().positive().optional(),
  max_batch_size: z.number().int().positive().optional(),
  max_files: z.number().int().positive().optional(),
  rerank_enabled: z.boolean().optional(),
  rerank_model: z.string().optional(),
  rerank_base_url: z.string().optional(),
  rerank_api_key_env: z.string().optional(),
  rerank_timeout_ms: z.number().optional(),
  rerank_max_candidates: z.number().optional(),
  rerank_max_candidate_chars: z.number().optional(),
  rerank_api_type: z.enum(["chat", "rerank"]).optional(),
  rerank_max_candidate_chars_cross_encoder: z.number().optional(),
  rerank_prompt_template: z.string().optional(),
  diagnostics_enabled: z.boolean().optional(),
  output_mode: z.enum(["off", "minimal", "verbose"]).optional(),
  query_prompt_template: z.string().optional(),
  document_prompt_template: z.string().optional(),
  use_model_profiles: z.boolean().optional(),
  max_results_per_file: z.number().optional(),
  max_embed_tokens: z.number().optional(),
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
  // Optional: overriding a built-in server (e.g. `rust`) to tweak one field
  // inherits the built-in's extensions/binary downstream. Requiring them here
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
   * edit/write call unless the tool call overrides diagnostics. Default: false.
   */
  diagnostics_on_edit: z.boolean().optional(),
  /**
   * Auto-install npm-distributed and GitHub-release language servers when
   * the project needs them. Default: true.
   */
  auto_install: z.boolean().optional(),
  /**
   * Supply-chain grace window. AFT only installs versions that have been on
   * the registry / GitHub releases for at least this many days. Default: 7.
   * User pins via `lsp.versions` bypass this.
   */
  // Audit-2 v0.17 #10: grace_days must be >= 1 because grace_days: 0 disables
  // the supply-chain grace window entirely with no warning. Users debugging
  // can still bypass the grace per-package via `lsp.versions` pins.
  grace_days: z.number().int().positive().optional(),
  /**
   * Per-package version pin map (npm package or GitHub repo).
   * Pins bypass the grace filter and any weekly version recheck.
   */
  versions: z.record(z.string().trim().min(1), z.string().trim().min(1)).optional(),
});

const ExperimentalConfigSchema = z.object({
  /**
   * @deprecated The bash family graduated from experimental in v0.27.2. Use
   * the top-level `bash` key instead. Still accepted for backward compat —
   * when present and top-level `bash` is absent, its values seed the
   * resolved bash config. Will be removed in v0.28.
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
 * Graduated `bash` config schema. Replaces `experimental.bash.*` in v0.27.2.
 * Three shapes: boolean (true/false) or partial object override.
 */
const BashFeaturesSchema = z.object({
  rewrite: z.boolean().optional(),
  compress: z.boolean().optional(),
  background: z.boolean().optional(),
  long_running_reminder_enabled: z.boolean().optional(),
  long_running_reminder_interval_ms: z.number().int().positive().optional(),
  foreground_wait_window_ms: z.number().int().positive().optional(),
});
const BashConfigSchema = z.union([z.boolean(), BashFeaturesSchema]);

const BridgeConfigSchema = z.object({
  request_timeout_ms: z
    .number()
    .int()
    .min(1000, { message: "bridge.request_timeout_ms must be at least 1000" })
    .optional(),
  hang_threshold: z
    .number()
    .int()
    .min(1, { message: "bridge.hang_threshold must be at least 1" })
    .optional(),
});

const InspectConfigSchema = z.object({
  enabled: z.boolean().optional(),
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
    format_on_edit: z.boolean().optional(),
    formatter_timeout_secs: z.number().int().min(1).max(600).optional(),
    validate_on_edit: z.enum(["syntax", "full"]).optional(),
    formatter: z.record(z.string(), FormatterEnum).optional(),
    checker: z.record(z.string(), CheckerEnum).optional(),
    configure_warnings_delivery: ConfigureWarningsDeliveryEnum.optional(),
    tool_surface: z.enum(["minimal", "recommended", "all"]).optional(),
    disabled_tools: z.array(z.string()).optional(),
    restrict_to_project_root: z.boolean().optional(),
    search_index: z.boolean().optional(),
    semantic_search: z.boolean().optional(),
    fts5: z
      .object({
        enabled: z.boolean().optional(),
        auto_index: z.boolean().optional(),
        index_on_start: z.boolean().optional(),
        max_results: z.number().optional(),
        max_body_chars: z.number().optional(),
        max_body_lines: z.number().optional(),
        raw_fts_debug: z.boolean().optional(),
      })
      .optional(),
    inspect: InspectConfigSchema.optional(),
    /**
     * Bash tool family (hoist + rewrite + compress + background execution).
     * Default on for `tool_surface: recommended`/`all`, off for `minimal`.
     * Three shapes: `true`, `false`, or `{ rewrite?, compress?, background?, ... }`.
     * Replaces `experimental.bash.*` (still accepted for backward compat).
     */
    bash: BashConfigSchema.optional(),
    experimental: ExperimentalConfigSchema.optional(),
    lsp: LspConfigSchema.optional(),
    url_fetch_allow_private: z.boolean().optional(),
    semantic: SemanticConfigSchema.optional(),
    max_callgraph_files: z.number().int().positive().optional(),
    bridge: BridgeConfigSchema.optional(),
  })
  .strict();

/** Config type derived from schema — single source of truth. */
export type AftConfig = z.infer<typeof AftConfigSchema>;

// ---------------------------------------------------------------------------
// Local helpers (AftConfig-specific — cannot be generic because they reference
// the plugin-specific schema)
// ---------------------------------------------------------------------------

const PROJECT_SAFE_TOP_LEVEL_FIELDS = new Set<keyof AftConfig>([
  "tool_surface",
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

function loadConfigFromPath(configPath: string): AftConfig | null {
  try {
    if (!existsSync(configPath)) return null;
    const content = readFileSync(configPath, "utf-8");
    const rawConfig = parseJsonc<Record<string, unknown>>(content);
    if (!rawConfig || typeof rawConfig !== "object" || Array.isArray(rawConfig)) {
      warn(`Config validation error in ${configPath}: root must be an object`);
      return null;
    }
    migrateRawConfig(rawConfig, configPath, { log, warn });
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

function parseConfigPartially(rawConfig: Record<string, unknown>): AftConfig {
  const partialConfig: Record<string, unknown> = {};
  const invalidSections: string[] = [];
  for (const key of Object.keys(rawConfig)) {
    const sectionResult = AftConfigSchema.safeParse({ [key]: rawConfig[key] });
    if (sectionResult.success) {
      const parsed = sectionResult.data as Record<string, unknown>;
      if (parsed[key] !== undefined) partialConfig[key] = parsed[key];
    } else {
      const sectionErrors = sectionResult.error.issues
        .filter((i) => i.path[0] === key)
        .map((i) => `${i.path.join(".")}: ${i.message}`)
        .join(", ");
      if (sectionErrors) invalidSections.push(`${key}: ${sectionErrors}`);
    }
  }
  if (invalidSections.length > 0) {
    warn(`Partial config loaded — invalid sections skipped: ${invalidSections.join("; ")}`);
  }
  return partialConfig as AftConfig;
}

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
// Pi config directory detection
//
// Pi's convention:
//   - Global: ~/.pi/agent/
//   - Project: <projectDir>/.pi/
// ---------------------------------------------------------------------------

function getGlobalPiDir(): string {
  return join(homedir(), ".pi", "agent");
}

/**
 * Load AFT config:
 *   1. User-level:    ~/.pi/agent/aft.jsonc (or .json)
 *   2. Project-level: <project>/.pi/aft.jsonc (or .json)
 *
 * Project config merges on top of user config.
 */
export function loadAftConfig(projectDirectory: string): AftConfig {
  const userBasePath = join(getGlobalPiDir(), "aft");
  migrateAftConfigFile(`${userBasePath}.jsonc`);
  migrateAftConfigFile(`${userBasePath}.json`);
  const userDetected = detectConfigFile(userBasePath);
  const userConfigPath =
    userDetected.format !== "none" ? userDetected.path : `${userBasePath}.json`;

  const projectBasePath = join(projectDirectory, ".pi", "aft");
  migrateAftConfigFile(`${projectBasePath}.jsonc`);
  migrateAftConfigFile(`${projectBasePath}.json`);
  const projectDetected = detectConfigFile(projectBasePath);
  const projectConfigPath =
    projectDetected.format !== "none" ? projectDetected.path : `${projectBasePath}.json`;

  let config: AftConfig = loadConfigFromPath(userConfigPath) ?? {};

  const projectConfig = loadConfigFromPath(projectConfigPath);
  if (projectConfig) {
    if (
      projectConfig.semantic?.backend !== undefined ||
      projectConfig.semantic?.base_url !== undefined ||
      projectConfig.semantic?.api_key_env !== undefined
    ) {
      warn(
        "Ignoring semantic.backend/base_url/api_key_env from project config (security: use user config for external backends)",
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
