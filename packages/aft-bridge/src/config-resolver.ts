/**
 * Shared config resolution and migration logic for AFT plugin hosts.
 *
 * Extracted from opencode-plugin and pi-plugin to eliminate ~600 lines of
 * duplicated code. Both hosts use the same bash resolver, config migration
 * engine, merge helpers, and bridge transport defaults.
 *
 * Plugin-specific code (Zod schemas, `loadAftConfig`, config directory
 * detection) remains in each plugin's own config.ts.
 */

import { existsSync, readFileSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import { parse as parseJsonc, stringify as stringifyJsonc } from "comment-json";
import { stripJsoncSymbols } from "./jsonc.js";
/**
 * Minimal log/warn callback surface consumed by shared helpers.
 * Callers pass their own host-specific logger (OpenCode or Pi).
 */
export interface ConfigLogger {
  log(message: string): void;
  warn(message: string): void;
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/** Default foreground wait-window before auto-promotion (ms). */
export const FOREGROUND_WAIT_WINDOW_DEFAULT_MS = 8_000;
/** Minimum allowed foreground wait-window (ms); smaller values clamp up. */
export const FOREGROUND_WAIT_WINDOW_MIN_MS = 5_000;

/** Default per-request bridge transport timeout in milliseconds. */
export const DEFAULT_BRIDGE_REQUEST_TIMEOUT_MS = 30_000;
/** Consecutive silent request timeouts before the bridge is killed and respawned. */
export const DEFAULT_BRIDGE_HANG_THRESHOLD = 2;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export type MigrationTarget = {
  oldKey: string;
  newPath: readonly string[];
};

export interface ConfigureLspServer {
  id: string;
  extensions?: string[];
  binary?: string;
  args: string[];
  root_markers: string[];
  disabled: boolean;
  env?: Record<string, string>;
  initialization_options?: unknown;
}

export interface ConfigureLspOverrides {
  experimental_lsp_ty?: boolean;
  lsp_servers?: ConfigureLspServer[];
  disabled_lsp?: string[];
}

export interface ConfigureExperimentalOverrides {
  experimental_bash_rewrite?: boolean;
  experimental_bash_compress?: boolean;
  experimental_bash_background?: boolean;
  bash_long_running_reminder_enabled?: boolean;
  bash_long_running_reminder_interval_ms?: number;
  experimental_lsp_ty?: boolean;
}

/**
 * Resolved bash configuration after merging top-level `bash`, the
 * legacy `experimental.bash.*` fallback, and tool_surface defaults.
 *
 * `enabled` controls hoist registration ONLY; the three sub-features
 * (rewrite/compress/background) are independent feature flags within
 * an enabled bash surface. `enabled: false` forces all three off and
 * disables hoist.
 */
export interface ResolvedBashConfig {
  enabled: boolean;
  rewrite: boolean;
  compress: boolean;
  background: boolean;
  /** Only valid in OpenCode (subagent-aware). Pi resolver omits this. */
  subagent_background?: boolean;
  long_running_reminder_enabled?: boolean;
  long_running_reminder_interval_ms?: number;
  /**
   * Foreground poll window before auto-promotion to background, in ms.
   * Always resolved: defaults to 8000, floored at 5000.
   */
  foreground_wait_window_ms: number;
}

/**
 * Minimal shape that every plugin's config must satisfy for the shared
 * resolvers. Each host defines its own full AftConfig type; this
 * interface captures only the fields consumed by the shared functions.
 */
export interface BashAwareConfig {
  bash?: boolean | Record<string, unknown>;
  experimental?: {
    bash?: {
      rewrite?: boolean;
      compress?: boolean;
      background?: boolean;
      long_running_reminder_enabled?: boolean;
      long_running_reminder_interval_ms?: number;
    };
    lsp_ty?: boolean;
  };
  tool_surface?: string;
}

export interface LspAwareConfig {
  lsp?: {
    servers?: Record<string, LspServerEntryConfig>;
    disabled?: string[];
    python?: "pyright" | "ty" | "auto";
    diagnostics_on_edit?: boolean;
  };
  experimental?: {
    lsp_ty?: boolean;
  };
}

export interface LspServerEntryConfig {
  extensions?: string[];
  binary?: string;
  args?: string[];
  root_markers?: string[];
  disabled?: boolean;
  env?: Record<string, string>;
  initialization_options?: unknown;
}

export interface SemanticLikeConfig {
  backend?: string;
  model?: string;
  base_url?: string;
  api_key_env?: string;
  timeout_ms?: number;
  max_batch_size?: number;
  max_files?: number;
  [key: string]: unknown;
}

export interface LspLikeConfig {
  servers?: Record<string, LspServerEntryConfig>;
  disabled?: string[];
  python?: "pyright" | "ty" | "auto";
  diagnostics_on_edit?: boolean;
  auto_install?: boolean;
  grace_days?: number;
  versions?: Record<string, string>;
  [key: string]: unknown;
}

export interface InspectLikeConfig {
  enabled?: boolean;
  duplicates?: {
    lower_bound?: number;
    discard_cost?: number;
    anonymize?: {
      variables?: boolean;
      fields?: boolean;
      methods?: boolean;
      types?: boolean;
      literals?: boolean;
    };
  };
  [key: string]: unknown;
}

export interface MergeableConfig {
  bash?: boolean | Record<string, unknown>;
  experimental?: {
    bash?: {
      rewrite?: boolean;
      compress?: boolean;
      background?: boolean;
      long_running_reminder_enabled?: boolean;
      long_running_reminder_interval_ms?: number;
    };
    lsp_ty?: boolean;
  };
  semantic?: SemanticLikeConfig;
  lsp?: LspLikeConfig;
  inspect?: InspectLikeConfig;
  disabled_tools?: string[];
  formatter?: Record<string, string>;
  checker?: Record<string, string>;
  tool_surface?: string;
  [key: string]: unknown;
}

// ---------------------------------------------------------------------------
// Bash config resolution
// ---------------------------------------------------------------------------

/**
 * Single source of truth for bash config across all plugin hosts. Resolution
 * order (highest priority wins):
 *
 *   1. Top-level `bash: false` → fully disabled (sub-features all false)
 *   2. Top-level `bash: true`  → fully enabled (sub-features all true)
 *   3. Top-level `bash: { ... }` → enabled; each sub-feature defaults true
 *      when not specified
 *   4. Top-level `bash` absent + any `experimental.bash.*` set → legacy
 *      fallback; sub-features take their explicit values (default false
 *      to preserve pre-v0.27.2 behavior — that block was opt-in)
 *   5. Top-level `bash` absent + no experimental → tool_surface default:
 *        - "minimal" → disabled
 *        - "recommended" or "all" → enabled with all sub-features on
 *
 * Set `subagentAware: true` in OpenCode hosts to include
 * `subagent_background` in the resolved config. Pi hosts should pass
 * `false`.
 */
export function resolveBashConfig(
  config: BashAwareConfig,
  opts: { subagentAware: boolean } = { subagentAware: false },
): ResolvedBashConfig {
  const top = config.bash;
  const legacy = config.experimental?.bash;
  const surface = config.tool_surface ?? "recommended";
  const surfaceDefaultEnabled = surface !== "minimal";

  const reminderEnabled =
    (typeof top === "object" && top !== null
      ? ((top as Record<string, unknown>).long_running_reminder_enabled as boolean | undefined)
      : undefined) ?? legacy?.long_running_reminder_enabled;
  const reminderInterval =
    (typeof top === "object" && top !== null
      ? ((top as Record<string, unknown>).long_running_reminder_interval_ms as number | undefined)
      : undefined) ?? legacy?.long_running_reminder_interval_ms;

  let topSubagentBg = false;
  if (opts.subagentAware && typeof top === "object" && top !== null) {
    topSubagentBg = (top as Record<string, unknown>).subagent_background === true;
  }

  const rawForegroundWait =
    typeof top === "object" && top !== null
      ? ((top as Record<string, unknown>).foreground_wait_window_ms as number | undefined)
      : undefined;
  const foregroundWaitWindowMs = Math.max(
    FOREGROUND_WAIT_WINDOW_MIN_MS,
    rawForegroundWait ?? FOREGROUND_WAIT_WINDOW_DEFAULT_MS,
  );

  const base: ResolvedBashConfig = {
    enabled: false,
    rewrite: false,
    compress: false,
    background: false,
    ...(opts.subagentAware ? { subagent_background: false } : {}),
    long_running_reminder_enabled: reminderEnabled,
    long_running_reminder_interval_ms: reminderInterval,
    foreground_wait_window_ms: foregroundWaitWindowMs,
  };

  if (top === false) return base;
  if (top === true) {
    return { ...base, enabled: true, rewrite: true, compress: true, background: true };
  }
  if (typeof top === "object" && top !== null) {
    const topObj = top as Record<string, unknown>;
    const result: ResolvedBashConfig = {
      ...base,
      enabled: true,
      rewrite: (topObj.rewrite as boolean) ?? true,
      compress: (topObj.compress as boolean) ?? true,
      background: (topObj.background as boolean) ?? true,
    };
    if (opts.subagentAware && topSubagentBg !== undefined) {
      result.subagent_background = topSubagentBg;
    }
    return result;
  }

  const hasLegacyFeatureFlag =
    legacy &&
    (legacy.rewrite !== undefined ||
      legacy.compress !== undefined ||
      legacy.background !== undefined);
  if (hasLegacyFeatureFlag) {
    const rewrite = legacy.rewrite === true;
    const compress = legacy.compress === true;
    const background = legacy.background === true;
    return { ...base, enabled: rewrite || compress || background, rewrite, compress, background };
  }

  return {
    ...base,
    enabled: surfaceDefaultEnabled,
    rewrite: surfaceDefaultEnabled,
    compress: surfaceDefaultEnabled,
    background: surfaceDefaultEnabled,
  };
}

// ---------------------------------------------------------------------------
// Experimental config resolution
// ---------------------------------------------------------------------------

export function resolveExperimentalConfigForConfigure(
  config: BashAwareConfig,
  subagentAware = false,
): ConfigureExperimentalOverrides {
  const overrides: ConfigureExperimentalOverrides = {};
  const bash = resolveBashConfig(config, { subagentAware });
  overrides.experimental_bash_rewrite = bash.rewrite;
  overrides.experimental_bash_compress = bash.compress;
  overrides.experimental_bash_background = bash.background;
  if (bash.long_running_reminder_enabled !== undefined) {
    overrides.bash_long_running_reminder_enabled = bash.long_running_reminder_enabled;
  }
  if (bash.long_running_reminder_interval_ms !== undefined) {
    overrides.bash_long_running_reminder_interval_ms = bash.long_running_reminder_interval_ms;
  }
  if (config.experimental?.lsp_ty !== undefined) {
    overrides.experimental_lsp_ty = config.experimental.lsp_ty;
  }
  return overrides;
}

// ---------------------------------------------------------------------------
// LSP config resolution
// ---------------------------------------------------------------------------

export function normalizeLspExtension(extension: string): string {
  return extension.trim().replace(/^\.+/, "");
}

export function resolveLspConfigForConfigure(config: LspAwareConfig): ConfigureLspOverrides {
  const overrides: ConfigureLspOverrides = {};
  const disabled = new Set(config.lsp?.disabled ?? []);
  let experimentalTy = config.experimental?.lsp_ty;

  switch (config.lsp?.python ?? "auto") {
    case "ty":
      experimentalTy = true;
      disabled.add("python");
      break;
    case "pyright":
      experimentalTy = false;
      disabled.add("ty");
      break;
    case "auto":
      break;
  }

  if (experimentalTy !== undefined) {
    overrides.experimental_lsp_ty = experimentalTy;
  }

  const servers = Object.entries(config.lsp?.servers ?? {}).map(([id, server]) => {
    const entry: ConfigureLspServer = {
      id,
      args: server.args ?? [],
      root_markers: server.root_markers ?? [".git"],
      disabled: server.disabled ?? false,
    };
    if (server.extensions && server.extensions.length > 0) {
      entry.extensions = server.extensions.map(normalizeLspExtension);
    }
    if (server.binary) {
      entry.binary = server.binary;
    }
    if (server.env && Object.keys(server.env).length > 0) {
      entry.env = server.env;
    }
    if (server.initialization_options !== undefined) {
      entry.initialization_options = server.initialization_options;
    }
    return entry;
  });
  if (servers.length > 0) {
    overrides.lsp_servers = servers;
  }

  if (disabled.size > 0) {
    overrides.disabled_lsp = [...disabled];
  }

  return overrides;
}

// ---------------------------------------------------------------------------
// Config migration
// ---------------------------------------------------------------------------

export const CONFIG_MIGRATIONS: readonly MigrationTarget[] = [
  { oldKey: "experimental_search_index", newPath: ["search_index"] },
  { oldKey: "experimental_semantic_search", newPath: ["semantic_search"] },
  { oldKey: "experimental_lsp_ty", newPath: ["experimental", "lsp_ty"] },
  { oldKey: "experimental_bash_rewrite", newPath: ["experimental", "bash", "rewrite"] },
  { oldKey: "experimental_bash_compress", newPath: ["experimental", "bash", "compress"] },
  { oldKey: "experimental_bash_background", newPath: ["experimental", "bash", "background"] },
];

function isWritableMigrationError(errorValue: unknown): boolean {
  const code = (errorValue as { code?: unknown })?.code;
  return code === "EROFS" || code === "EACCES" || code === "EPERM";
}

export function extractCommentsForPreservation(content: string): string[] {
  const comments: string[] = [];
  const linePattern = /\/\/[^\n]*/g;
  for (const match of content.match(linePattern) ?? []) {
    comments.push(match.trim());
  }
  const blockPattern = /\/\*[\s\S]*?\*\//g;
  for (const match of content.match(blockPattern) ?? []) {
    comments.push(match.replace(/\s+/g, " ").trim());
  }
  return comments;
}

export function ensureRecordAtPath(
  root: Record<string, unknown>,
  path: readonly string[],
): Record<string, unknown> {
  let current = root;
  for (const segment of path) {
    const existing = current[segment];
    if (!existing || typeof existing !== "object" || Array.isArray(existing)) {
      current[segment] = {};
    }
    current = current[segment] as Record<string, unknown>;
  }
  return current;
}

export function hasPath(root: Record<string, unknown>, path: readonly string[]): boolean {
  let current: unknown = root;
  for (const segment of path) {
    if (!current || typeof current !== "object" || Array.isArray(current)) return false;
    const record = current as Record<string, unknown>;
    if (!Object.hasOwn(record, segment)) return false;
    current = record[segment];
  }
  return true;
}

export function setPath(
  root: Record<string, unknown>,
  path: readonly string[],
  value: unknown,
): void {
  const parent = ensureRecordAtPath(root, path.slice(0, -1));
  parent[path[path.length - 1]] = value;
}

export function migrateRawConfig(
  rawConfig: Record<string, unknown>,
  configPath: string,
  logger?: ConfigLogger,
): string[] {
  const oldKeys: string[] = [];
  for (const migration of CONFIG_MIGRATIONS) {
    if (!Object.hasOwn(rawConfig, migration.oldKey)) continue;

    if (hasPath(rawConfig, migration.newPath)) {
      logger?.warn(
        `Config migration conflict at ${configPath}: ${migration.oldKey} ignored because ${migration.newPath.join(".")} is already set`,
      );
    } else {
      setPath(rawConfig, migration.newPath, rawConfig[migration.oldKey]);
    }
    delete rawConfig[migration.oldKey];
    oldKeys.push(migration.oldKey);
  }
  oldKeys.push(...migrateExperimentalBashBlock(rawConfig, configPath, logger));
  return oldKeys;
}

export function migrateExperimentalBashBlock(
  rawConfig: Record<string, unknown>,
  configPath: string,
  logger?: ConfigLogger,
): string[] {
  const experimental = rawConfig.experimental;
  if (typeof experimental !== "object" || experimental === null || Array.isArray(experimental)) {
    return [];
  }
  const expRecord = experimental as Record<string, unknown>;
  if (!Object.hasOwn(expRecord, "bash")) return [];

  const legacyBash = expRecord.bash;

  if (typeof legacyBash !== "object" || legacyBash === null || Array.isArray(legacyBash)) {
    delete expRecord.bash;
    if (Object.keys(expRecord).length === 0) delete rawConfig.experimental;
    return ["experimental.bash"];
  }

  const bashRecord = legacyBash as Record<string, unknown>;
  const hasFeatureFlag =
    "rewrite" in bashRecord || "compress" in bashRecord || "background" in bashRecord;

  if (!hasFeatureFlag) return [];

  const movedKeys = Object.keys(bashRecord).map((k) => `experimental.bash.${k}`);

  if (Object.hasOwn(rawConfig, "bash")) {
    logger?.warn(
      `Config migration conflict at ${configPath}: experimental.bash dropped because top-level "bash" is already set`,
    );
  } else {
    const migrated: Record<string, unknown> = {
      rewrite: bashRecord.rewrite === true,
      compress: bashRecord.compress === true,
      background: bashRecord.background === true,
    };
    if (bashRecord.long_running_reminder_enabled !== undefined) {
      migrated.long_running_reminder_enabled = bashRecord.long_running_reminder_enabled;
    }
    if (bashRecord.long_running_reminder_interval_ms !== undefined) {
      migrated.long_running_reminder_interval_ms = bashRecord.long_running_reminder_interval_ms;
    }
    rawConfig.bash = migrated;
  }
  delete expRecord.bash;

  if (Object.keys(expRecord).length === 0) {
    delete rawConfig.experimental;
  }

  return movedKeys;
}

export function migrateAftConfigFile(
  configPath: string,
  logger?: ConfigLogger,
): { migrated: boolean; oldKeys: string[] } {
  if (!existsSync(configPath)) {
    return { migrated: false, oldKeys: [] };
  }

  let tmpPath: string | null = null;
  let oldKeys: string[] = [];
  try {
    const content = readFileSync(configPath, "utf-8");
    const rawConfig = parseJsonc<Record<string, unknown>>(content);
    if (!rawConfig || typeof rawConfig !== "object" || Array.isArray(rawConfig)) {
      return { migrated: false, oldKeys: [] };
    }

    oldKeys = migrateRawConfig(rawConfig, configPath, logger);
    if (oldKeys.length === 0) {
      return { migrated: false, oldKeys: [] };
    }

    const serialized = `${stringifyJsonc(rawConfig, null, 2)}\n`;
    const originalComments = extractCommentsForPreservation(content);
    const droppedComments = originalComments.filter(
      (comment) => !serialized.includes(comment.trim()),
    );
    const nextContent =
      droppedComments.length > 0 ? `${droppedComments.join("\n")}\n${serialized}` : serialized;

    tmpPath = `${configPath}.tmp.${process.pid}`;
    writeFileSync(tmpPath, nextContent, "utf-8");
    renameSync(tmpPath, configPath);
    logger?.log?.(`Migrated config at ${configPath}: removed ${oldKeys.join(", ")}`);
    return { migrated: true, oldKeys };
  } catch (err) {
    if (tmpPath) {
      try {
        unlinkSync(tmpPath);
      } catch {
        // best-effort cleanup
      }
    }
    if (isWritableMigrationError(err)) {
      const errorMsg = err instanceof Error ? err.message : String(err);
      logger?.warn?.(
        `Config migration could not write ${configPath} (${errorMsg}); using migrated config in memory`,
      );
      return { migrated: oldKeys.length > 0, oldKeys };
    }
    return { migrated: false, oldKeys: [] };
  }
}

// ---------------------------------------------------------------------------
// Config merge helpers
// ---------------------------------------------------------------------------

export function mergeSemanticConfig(
  base:
    | {
        backend?: string;
        model?: string;
        base_url?: string;
        api_key_env?: string;
        timeout_ms?: number;
        max_batch_size?: number;
        max_files?: number;
        [key: string]: unknown;
      }
    | undefined,
  override:
    | {
        backend?: string;
        model?: string;
        base_url?: string;
        api_key_env?: string;
        timeout_ms?: number;
        max_batch_size?: number;
        max_files?: number;
        [key: string]: unknown;
      }
    | undefined,
): Record<string, unknown> | undefined {
  const projectSafe: Record<string, unknown> = {};
  if (override?.model !== undefined) projectSafe.model = override.model;
  if (override?.timeout_ms !== undefined) projectSafe.timeout_ms = override.timeout_ms;
  if (override?.max_batch_size !== undefined) projectSafe.max_batch_size = override.max_batch_size;
  if (override?.max_files !== undefined) projectSafe.max_files = override.max_files;

  const semantic = { ...base, ...projectSafe };
  if (Object.values(semantic).every((v) => v === undefined)) return undefined;

  return Object.fromEntries(Object.entries(semantic).filter(([, v]) => v !== undefined));
}

export function mergeLspConfig(
  base: LspLikeConfig | undefined,
  override: LspLikeConfig | undefined,
): LspLikeConfig | undefined {
  const projectSafe: LspLikeConfig = {};
  if (override?.python !== undefined) projectSafe.python = override.python;
  if (override?.diagnostics_on_edit !== undefined) {
    projectSafe.diagnostics_on_edit = override.diagnostics_on_edit;
  }

  const userDisabled = base?.disabled ?? [];
  const lsp: LspLikeConfig = {
    ...base,
    ...projectSafe,
    ...(userDisabled.length > 0 ? { disabled: [...userDisabled] } : {}),
  };

  if (Object.values(lsp).every((v) => v === undefined)) return undefined;

  return Object.fromEntries(
    Object.entries(lsp).filter(([, v]) => v !== undefined),
  ) as LspLikeConfig;
}

export function mergeInspectConfig(
  base: InspectLikeConfig | undefined,
  override: InspectLikeConfig | undefined,
): InspectLikeConfig | undefined {
  const inspect: InspectLikeConfig = {
    ...base,
    ...override,
    duplicates:
      base?.duplicates || override?.duplicates
        ? {
            ...base?.duplicates,
            ...override?.duplicates,
            anonymize:
              base?.duplicates?.anonymize || override?.duplicates?.anonymize
                ? {
                    ...base?.duplicates?.anonymize,
                    ...override?.duplicates?.anonymize,
                  }
                : undefined,
          }
        : undefined,
  };

  if (inspect.duplicates && inspect.duplicates.anonymize === undefined) {
    delete inspect.duplicates.anonymize;
  }
  if (Object.values(inspect).every((v) => v === undefined)) return undefined;
  return Object.fromEntries(
    Object.entries(inspect).filter(([, v]) => v !== undefined),
  ) as InspectLikeConfig;
}

export function mergeBashConfig(
  base: boolean | Record<string, unknown> | undefined,
  overwrite: boolean | Record<string, unknown> | undefined,
): Record<string, unknown> | undefined {
  if (base === undefined && overwrite === undefined) return undefined;
  if (base === undefined) return overwrite === undefined ? undefined : mergeExpand(overwrite);
  if (overwrite === undefined) return mergeExpand(base);

  return { ...mergeExpand(base), ...mergeExpand(overwrite) };
}

function mergeExpand(value: boolean | Record<string, unknown>): Record<string, unknown> {
  if (value === true) return { rewrite: true, compress: true, background: true };
  if (value === false) return { rewrite: false, compress: false, background: false };
  return { ...(value ?? {}) };
}

export function mergeExperimentalConfig(
  base: Record<string, unknown> | undefined,
  override: Record<string, unknown> | undefined,
): Record<string, unknown> | undefined {
  const bash: Record<string, unknown> = {
    ...(base?.bash as Record<string, unknown>),
    ...(override?.bash as Record<string, unknown>),
  };
  const experimental: Record<string, unknown> = { ...base, ...override };

  if (Object.values(bash).some((v) => v !== undefined)) {
    experimental.bash = bash;
  } else {
    delete experimental.bash;
  }
  if (Object.values(experimental).every((v) => v === undefined)) return undefined;

  return Object.fromEntries(Object.entries(experimental).filter(([, v]) => v !== undefined));
}

export function getProjectLspStrippedKeys(lsp: LspLikeConfig | undefined): string[] {
  if (!lsp) return [];
  const stripped: string[] = [];
  if (lsp.servers !== undefined) stripped.push("lsp.servers");
  if (lsp.versions !== undefined) stripped.push("lsp.versions");
  if (lsp.auto_install !== undefined) stripped.push("lsp.auto_install");
  if (lsp.grace_days !== undefined) stripped.push("lsp.grace_days");
  if (lsp.disabled !== undefined) stripped.push("lsp.disabled");
  return stripped;
}

// ---------------------------------------------------------------------------
// Config file detection
// ---------------------------------------------------------------------------

export function detectConfigFile(basePath: string): {
  format: "json" | "jsonc" | "none";
  path: string;
} {
  const jsoncPath = `${basePath}.jsonc`;
  const jsonPath = `${basePath}.json`;

  if (existsSync(jsoncPath)) return { format: "jsonc", path: jsoncPath };
  if (existsSync(jsonPath)) return { format: "json", path: jsonPath };
  return { format: "none", path: jsonPath };
}

// ---------------------------------------------------------------------------
// Bridge transport defaults
// ---------------------------------------------------------------------------

export function resolveBridgePoolTransportOptions(config: {
  bridge?: { request_timeout_ms?: number; hang_threshold?: number };
}): { timeoutMs: number; hangThreshold: number } {
  return {
    timeoutMs: config.bridge?.request_timeout_ms ?? DEFAULT_BRIDGE_REQUEST_TIMEOUT_MS,
    hangThreshold: config.bridge?.hang_threshold ?? DEFAULT_BRIDGE_HANG_THRESHOLD,
  };
}
