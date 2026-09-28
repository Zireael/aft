/**
 * Live reload of AFT config file edits in a host plugin.
 *
 * The plugin keeps its own copy of the resolved config (`ctx.config`) for the
 * keys it enforces or uses itself, such as the path restriction pre-check and
 * the bash wait limits. The engine applies its own keys from the same files
 * separately, so this module only swaps the plugin's copy.
 *
 * Only keys that are safe to change under a running session are applied: the
 * ones the plugin reads from `ctx.config` on every tool call. Keys that change
 * the registered tools, their descriptions or the system text stay as loaded
 * and apply at the next host restart; they are reported as deferred.
 */

import { existsSync, type FSWatcher, readFileSync, watch } from "node:fs";
import { dirname } from "node:path";

/** How long a burst of file events must be quiet before the files are read. */
export const CONFIG_WATCH_DEBOUNCE_MS = 150;

/**
 * Appended to a config error found by a live reload, in place of the
 * "restart the host" note a startup config error carries: the plugin keeps
 * running on the last valid configuration.
 */
export const CONFIG_LIVE_KEEP_NOTE =
  "AFT keeps using the last valid configuration until the file is fixed.";

/** One config key the plugin applies live: how to read it and write it. */
export interface LiveConfigKey<C> {
  /** Dotted config key, as the user writes it in aft.jsonc. */
  name: string;
  read(config: C): unknown;
  /** Return a copy of `config` with this key set to `value`. */
  write(config: C, value: unknown): C;
}

/** The bash settings this module needs from a host's `resolveBashConfig`. */
export interface ResolvedBashForLiveReload {
  foreground_wait_window_ms: number;
  host_fallback: boolean;
  subagent_background: boolean;
  watch_sync_max_ms: number;
}

type AnyConfig = Record<string, unknown>;

function pathKey<C>(name: string): LiveConfigKey<C> {
  const segments = name.split(".");
  return {
    name,
    read(config) {
      let value: unknown = config;
      for (const segment of segments) {
        if (value === null || typeof value !== "object") return undefined;
        value = (value as AnyConfig)[segment];
      }
      return value;
    },
    write(config, value) {
      const set = (target: unknown, index: number): AnyConfig => {
        const copy: AnyConfig =
          target !== null && typeof target === "object" && !Array.isArray(target)
            ? { ...(target as AnyConfig) }
            : {};
        const segment = segments[index] as string;
        if (index === segments.length - 1) {
          if (value === undefined) delete copy[segment];
          else copy[segment] = value;
        } else {
          copy[segment] = set(copy[segment], index + 1);
        }
        return copy;
      };
      return set(config, 0) as C;
    },
  };
}

/**
 * Bash keys are read through the host's `resolveBashConfig`, because `bash`
 * may be a boolean, an object or the legacy `experimental.bash` block. A write
 * turns `bash` into the equivalent object form, so every other bash setting
 * (for example `compress` or `background`, which are not live) resolves to
 * the same value as before.
 */
function bashKey<C>(
  name: keyof ResolvedBashForLiveReload,
  resolveBash: (config: C) => ResolvedBashForLiveReload & Record<string, unknown>,
): LiveConfigKey<C> {
  return {
    name: `bash.${name}`,
    read: (config) => resolveBash(config)[name],
    write(config, value) {
      const resolved: Record<string, unknown> = { ...resolveBash(config), [name]: value };
      for (const key of Object.keys(resolved)) {
        if (resolved[key] === undefined) delete resolved[key];
      }
      return { ...(config as AnyConfig), bash: resolved } as C;
    },
  };
}

/**
 * Every key the plugins read that a live reload applies. The engine-side
 * list is `apply_live_config` in `crates/aft/src/config_live.rs`.
 */
export function aftLiveConfigKeys<C>(
  resolveBash: (config: C) => ResolvedBashForLiveReload & Record<string, unknown>,
): LiveConfigKey<C>[] {
  return [
    pathKey<C>("configure_warnings_delivery"),
    pathKey<C>("restrict_to_project_root"),
    pathKey<C>("inspect.diagnostics_timeout_ms"),
    pathKey<C>("inspect.tier2_idle_minutes"),
    bashKey<C>("foreground_wait_window_ms", resolveBash),
    bashKey<C>("host_fallback", resolveBash),
    bashKey<C>("subagent_background", resolveBash),
    bashKey<C>("watch_sync_max_ms", resolveBash),
  ];
}

function sameValue(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

function flattenLeaves(value: unknown, prefix: string, out: Map<string, string>): void {
  if (value !== null && typeof value === "object" && !Array.isArray(value)) {
    const entries = Object.entries(value as AnyConfig);
    if (entries.length === 0 && prefix) out.set(prefix, "{}");
    for (const [key, child] of entries) {
      flattenLeaves(child, prefix ? `${prefix}.${key}` : key, out);
    }
    return;
  }
  if (prefix) out.set(prefix, JSON.stringify(value));
}

/** What {@link applyLiveConfigKeys} changed and what it left for a restart. */
export interface LiveConfigApply<C> {
  /** `current` with only the changed live keys taken from `next`. */
  config: C;
  applied: string[];
  deferred: string[];
}

/**
 * Copy the live keys that differ from `current` out of `next`. `config` is a
 * new object whenever something was applied, so a caller that captured the
 * old one keeps a consistent snapshot. Other settings that differ from
 * `baseline` (the config the plugin loaded at startup) are listed in
 * `deferred` and not applied.
 */
export function applyLiveConfigKeys<C>(
  current: C,
  next: C,
  keys: readonly LiveConfigKey<C>[],
  baseline: C = current,
): LiveConfigApply<C> {
  let config = current;
  const applied: string[] = [];
  for (const key of keys) {
    const value = key.read(next);
    if (sameValue(key.read(current), value)) continue;
    config = key.write(config, value);
    applied.push(key.name);
  }

  const before = new Map<string, string>();
  const after = new Map<string, string>();
  flattenLeaves(baseline, "", before);
  flattenLeaves(next, "", after);
  const liveNames = keys.map((key) => key.name);
  const isLive = (leaf: string): boolean =>
    liveNames.some((name) => leaf === name || leaf.startsWith(`${name}.`));
  const deferred = new Set<string>();
  for (const leaf of new Set([...before.keys(), ...after.keys()])) {
    if (before.get(leaf) === after.get(leaf) || isLive(leaf)) continue;
    deferred.add(leaf);
  }
  return { config, applied, deferred: [...deferred].sort() };
}

/** Options for {@link watchAftConfigFiles}. */
export interface WatchAftConfigFilesOptions {
  /** The config files to watch (user and project `aft.jsonc`). */
  paths: readonly string[];
  /** Called after a quiet debounce window when any file's text changed. */
  onChange: () => void;
  debounceMs?: number;
}

function readTextOrNull(path: string): string | null {
  try {
    return readFileSync(path, "utf8");
  } catch {
    return null;
  }
}

/**
 * Watch config files the way editors save them: the parent directory is
 * watched, because an editor replaces the file by renaming a temporary
 * sibling over it. A directory that does not exist yet is watched through its
 * own parent until it appears. `onChange` runs only when a file's text
 * actually differs from what was last seen. Returns a function that stops
 * every watch.
 */
export function watchAftConfigFiles(options: WatchAftConfigFilesOptions): () => void {
  const debounceMs = options.debounceMs ?? CONFIG_WATCH_DEBOUNCE_MS;
  const lastSeen = new Map<string, string | null>();
  for (const path of options.paths) lastSeen.set(path, readTextOrNull(path));
  let timer: ReturnType<typeof setTimeout> | null = null;
  let stopped = false;
  const watchers = new Map<string, FSWatcher>();

  const check = (): void => {
    timer = null;
    if (stopped) return;
    attachAll();
    let changed = false;
    for (const path of options.paths) {
      const text = readTextOrNull(path);
      if (text !== lastSeen.get(path)) {
        lastSeen.set(path, text);
        changed = true;
      }
    }
    if (changed) options.onChange();
  };
  const schedule = (): void => {
    if (stopped) return;
    if (timer) clearTimeout(timer);
    timer = setTimeout(check, debounceMs);
    timer.unref?.();
  };

  const attach = (dir: string): void => {
    if (watchers.has(dir)) return;
    try {
      const watcher = watch(dir, { persistent: false }, () => schedule());
      watcher.on("error", () => {
        watcher.close();
        watchers.delete(dir);
      });
      watchers.set(dir, watcher);
    } catch {
      // The directory is missing or unreadable; its parent is tried instead.
      const parent = dirname(dir);
      if (parent !== dir) attach(parent);
    }
  };
  function attachAll(): void {
    for (const path of options.paths) attach(dirname(path));
  }
  attachAll();

  return () => {
    stopped = true;
    if (timer) clearTimeout(timer);
    for (const watcher of watchers.values()) watcher.close();
    watchers.clear();
  };
}

/** Result of loading the config files for a live reload. */
export type LiveConfigLoad<C> = { ok: true; config: C } | { ok: false; message: string };

/** Options for {@link startLiveConfigReload}. */
export interface LiveConfigReloadOptions<C> {
  /** The user and project `aft.jsonc` files. */
  paths: readonly string[];
  /** Load and resolve both files. Anything but a clean load is `ok: false`. */
  load(): LiveConfigLoad<C>;
  keys: readonly LiveConfigKey<C>[];
  getConfig(): C;
  setConfig(config: C): void;
  /** Informational log line. */
  log(message: string): void;
  /** A config error, logged and shown to the user once per distinct message. */
  reportError(message: string): void;
  /** Skip the file watch; tests call `reload()` directly. */
  watch?: boolean;
  debounceMs?: number;
}

/** Handle for a running live config reload. */
export interface LiveConfigReload {
  /** Read the files now and apply the live keys that changed. */
  reload(): LiveConfigApply<unknown> | null;
  stop(): void;
}

/** The log line a reload writes, in the same shape as the engine's. */
export function liveConfigReloadLogLine(applied: string[], deferred: string[]): string {
  let line = "config reload";
  if (applied.length > 0) line += ` applied=[${applied.join(",")}]`;
  if (deferred.length > 0) {
    line += ` deferred=[${deferred.join(",")}] (deferred keys apply on next connect/restart)`;
  }
  return line;
}

/**
 * Watch the config files and keep `ctx.config` current for the live keys.
 * An invalid, unreadable or deleted file keeps the last valid config: the
 * error is reported and nothing else changes.
 */
export function startLiveConfigReload<C>(options: LiveConfigReloadOptions<C>): LiveConfigReload {
  const baseline = options.getConfig();
  let lastError: string | null = null;
  // Files that existed at the last valid load. A deleted one keeps the last
  // valid config rather than resolving as empty: a security setting must not
  // loosen because a file vanished mid-save or was deleted by mistake.
  const present = new Map(options.paths.map((path) => [path, existsSync(path)]));
  const reload = (): LiveConfigApply<C> | null => {
    const deleted = options.paths.find((path) => present.get(path) === true && !existsSync(path));
    const loaded: LiveConfigLoad<C> =
      deleted !== undefined
        ? { ok: false, message: `AFT config at ${deleted} was deleted` }
        : options.load();
    if (!loaded.ok) {
      const message = `${loaded.message.trim().replace(/[.!?]?$/, ".")} ${CONFIG_LIVE_KEEP_NOTE}`;
      if (message !== lastError) {
        lastError = message;
        options.reportError(message);
      }
      return null;
    }
    lastError = null;
    for (const path of options.paths) present.set(path, existsSync(path));
    const result = applyLiveConfigKeys(options.getConfig(), loaded.config, options.keys, baseline);
    if (result.applied.length > 0) options.setConfig(result.config);
    if (result.applied.length > 0 || result.deferred.length > 0) {
      options.log(liveConfigReloadLogLine(result.applied, result.deferred));
    }
    return result;
  };
  const stop =
    options.watch === false
      ? () => {}
      : watchAftConfigFiles({
          paths: options.paths,
          debounceMs: options.debounceMs,
          onChange: () => {
            try {
              reload();
            } catch (err) {
              options.reportError(
                `AFT config reload failed: ${err instanceof Error ? err.message : String(err)}`,
              );
            }
          },
        });
  return { reload: reload as () => LiveConfigApply<unknown> | null, stop };
}
