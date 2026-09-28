import { existsSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { execFile, getOpenCodeCacheRoot } from "@cortexkit/aft-bridge";
import { parse as parseJsonc } from "comment-json";

import { debug, log, warn } from "../../logger.js";
import {
  cacheDir,
  NPM_FETCH_TIMEOUT,
  NPM_REGISTRY_URL,
  PACKAGE_NAME,
  userOpenCodeConfig,
  userOpenCodeConfigJsonc,
} from "./constants.js";
import {
  NpmPackageEnvelopeSchema,
  OpencodeConfigSchema,
  PackageJsonSchema,
  type PluginEntryInfo,
} from "./types.js";

function isString(value: unknown): value is string {
  return typeof value === "string";
}

function pluginSpecifier(entry: string | readonly [string, Record<string, unknown>]): string {
  return typeof entry === "string" ? entry : entry[0];
}

function getPluginEntries(config: unknown): string[] {
  const parsed = OpencodeConfigSchema.safeParse(config);
  if (!parsed.success) return [];
  return (parsed.data.plugin ?? []).map(pluginSpecifier).filter(isString);
}

function parseJsonConfig(content: string): unknown | null {
  try {
    return parseJsonc(content);
  } catch (err) {
    warn(`[auto-update-checker] Failed to parse OpenCode config: ${String(err)}`);
    return null;
  }
}

function isPrereleaseVersion(version: string): boolean {
  return version.includes("-");
}

function isDistTag(version: string): boolean {
  return !/^\d/.test(version);
}

export function extractChannel(version: string | null): string {
  if (!version) return "latest";

  if (isDistTag(version)) return version;

  if (isPrereleaseVersion(version)) {
    const prereleasePart = version.split("-")[1];
    const channelMatch = prereleasePart?.match(/^(alpha|beta|rc|canary|next)/);
    if (channelMatch?.[1]) return channelMatch[1];
  }

  return "latest";
}

function getConfigPaths(directory: string): string[] {
  return [
    join(directory, ".opencode", "opencode.json"),
    join(directory, ".opencode", "opencode.jsonc"),
    userOpenCodeConfig(),
    userOpenCodeConfigJsonc(),
  ];
}

function resolvePathPluginSpec(spec: string, configPath: string): string {
  if (spec.startsWith("file://")) {
    try {
      return fileURLToPath(spec);
    } catch {
      return spec.replace(/^file:\/\//, "");
    }
  }
  if (isAbsolute(spec) || /^[A-Za-z]:[\\/]/.test(spec)) return spec;
  return resolve(dirname(configPath), spec);
}

function getLocalDevPath(directory: string): string | null {
  for (const configPath of getConfigPaths(directory)) {
    try {
      if (!existsSync(configPath)) continue;
      const rawConfig = parseJsonConfig(readFileSync(configPath, "utf-8"));
      const plugins = getPluginEntries(rawConfig);

      for (const entry of plugins) {
        if (entry === PACKAGE_NAME || entry.startsWith(`${PACKAGE_NAME}@`)) continue;
        if (entry.startsWith("file://") || entry.startsWith(".") || isAbsolute(entry)) {
          const localPath = resolvePathPluginSpec(entry, configPath);
          const pkgPath = findPackageJsonUp(localPath);
          if (!pkgPath) continue;
          const pkg = PackageJsonSchema.safeParse(JSON.parse(readFileSync(pkgPath, "utf-8")));
          if (pkg.success && pkg.data.name === PACKAGE_NAME) return localPath;
        }
      }
    } catch {
      // Config probing must never block plugin startup.
    }
  }
  return null;
}

function findPackageJsonUp(startPath: string): string | null {
  try {
    const stat = statSync(startPath);
    let dir = stat.isDirectory() ? startPath : dirname(startPath);

    for (let i = 0; i < 10; i++) {
      const pkgPath = join(dir, "package.json");
      if (existsSync(pkgPath)) {
        try {
          const pkg = PackageJsonSchema.safeParse(JSON.parse(readFileSync(pkgPath, "utf-8")));
          if (pkg.success && pkg.data.name === PACKAGE_NAME) return pkgPath;
        } catch {
          // Continue walking upward.
        }
      }
      const parent = dirname(dir);
      if (parent === dir) break;
      dir = parent;
    }
  } catch {
    // Missing path or unreadable package metadata.
  }
  return null;
}

export function getLocalDevVersion(directory: string): string | null {
  const localPath = getLocalDevPath(directory);
  if (!localPath) return null;

  try {
    const pkgPath = findPackageJsonUp(localPath);
    if (!pkgPath) return null;
    const pkg = PackageJsonSchema.safeParse(JSON.parse(readFileSync(pkgPath, "utf-8")));
    return pkg.success ? (pkg.data.version ?? null) : null;
  } catch {
    return null;
  }
}

export function getCurrentRuntimePackageJsonPath(
  currentModuleUrl: string = import.meta.url,
): string | null {
  try {
    return findPackageJsonUp(dirname(fileURLToPath(currentModuleUrl)));
  } catch (err) {
    warn(`[auto-update-checker] Failed to resolve runtime package path: ${String(err)}`);
    return null;
  }
}

export function findPluginEntry(directory: string): PluginEntryInfo | null {
  for (const configPath of getConfigPaths(directory)) {
    try {
      if (!existsSync(configPath)) continue;
      const rawConfig = parseJsonConfig(readFileSync(configPath, "utf-8"));
      const plugins = getPluginEntries(rawConfig);

      for (const entry of plugins) {
        if (entry === PACKAGE_NAME) {
          return { entry, isPinned: false, pinnedVersion: null, configPath };
        }
        if (entry.startsWith(`${PACKAGE_NAME}@`)) {
          const pinnedVersion = entry.slice(PACKAGE_NAME.length + 1);
          const isPinned = pinnedVersion !== "latest";
          return { entry, isPinned, pinnedVersion: isPinned ? pinnedVersion : null, configPath };
        }
      }
    } catch {
      // Ignore unreadable configs and keep scanning lower-priority paths.
    }
  }
  return null;
}

let cachedPackageVersion: string | null = null;

function getSpecCachePackageJsonPath(spec: string): string {
  return join(cacheDir(), spec, "node_modules", PACKAGE_NAME, "package.json");
}

export function getCachedVersion(spec?: string | null): string | null {
  if (!spec && cachedPackageVersion) return cachedPackageVersion;

  const candidates = [
    getCurrentRuntimePackageJsonPath(),
    spec ? getSpecCachePackageJsonPath(spec) : null,
    getSpecCachePackageJsonPath(`${PACKAGE_NAME}@latest`),
    join(getOpenCodeCacheRoot(), "node_modules", PACKAGE_NAME, "package.json"),
  ].filter(isString);

  for (const packageJsonPath of candidates) {
    try {
      if (!existsSync(packageJsonPath)) continue;
      const pkg = PackageJsonSchema.safeParse(JSON.parse(readFileSync(packageJsonPath, "utf-8")));
      if (pkg.success && pkg.data.version) {
        if (!spec) cachedPackageVersion = pkg.data.version;
        return pkg.data.version;
      }
    } catch {
      // Try the next known OpenCode cache location.
    }
  }

  return null;
}

export function updatePinnedVersion(
  configPath: string,
  oldEntry: string,
  newVersion: string,
): boolean {
  try {
    if (!existsSync(configPath)) return false;

    const content = readFileSync(configPath, "utf-8");
    const newEntry = `${PACKAGE_NAME}@${newVersion}`;
    const escapedOldEntry = oldEntry.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const entryRegex = new RegExp(`(["'])${escapedOldEntry}\\1`, "g");

    if (!entryRegex.test(content)) {
      log(`[auto-update-checker] Entry "${oldEntry}" not found in ${configPath}`);
      return false;
    }

    const updatedContent = content.replace(entryRegex, `$1${newEntry}$1`);
    if (updatedContent === content) return false;

    writeFileSync(configPath, updatedContent, "utf-8");
    log(`[auto-update-checker] Updated ${configPath}: ${oldEntry} → ${newEntry}`);
    return true;
  } catch (err) {
    warn(`[auto-update-checker] Failed to update config file ${configPath}: ${String(err)}`);
    return false;
  }
}

const registryCache = new Map<string, Promise<string>>();

function npmConfigGet(key: string, directory: string): Promise<string> {
  return new Promise((resolve, reject) => {
    execFile(
      process.platform === "win32" ? "npm.cmd" : "npm",
      ["config", "get", key],
      {
        cwd: directory,
        timeout: NPM_FETCH_TIMEOUT,
        windowsHide: true,
        shell: process.platform === "win32",
      },
      (error, stdout) => {
        if (error) reject(error);
        else resolve(stdout.trim());
      },
    );
  });
}

function configuredRegistry(value: string): string | undefined {
  return value && value !== "undefined" && value !== "null" ? value : undefined;
}

type RegistrySettings = { registry?: string; "@cortexkit:registry"?: string };

async function readRegistrySettings(path: string, required = false): Promise<RegistrySettings> {
  let text: string;
  try {
    text = await readFile(path, "utf8");
  } catch (error) {
    if (!required && (error as NodeJS.ErrnoException).code === "ENOENT") return {};
    throw error;
  }
  const settings: RegistrySettings = {};
  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#") || trimmed.startsWith(";")) continue;
    const separator = trimmed.indexOf("=");
    if (separator < 0) continue;
    const key = trimmed.slice(0, separator).trim();
    if (key === "registry" || key === "@cortexkit:registry") {
      settings[key] = configuredRegistry(trimmed.slice(separator + 1).trim());
    }
  }
  return settings;
}

function registryEnvironment(key: string): string | undefined {
  const variable = `npm_config_${key}`;
  const value =
    process.env[variable] ??
    Object.entries(process.env).find(([name]) => name.toLowerCase() === variable)?.[1];
  return configuredRegistry(value ?? "");
}

async function registryWithoutNpm(directory: string): Promise<string> {
  let scoped = registryEnvironment("@cortexkit:registry");
  let registry = registryEnvironment("registry");
  const visited = new Set<string>();
  for (let current = directory; ; current = dirname(current)) {
    if (scoped) break;
    const path = join(current, ".npmrc");
    visited.add(path);
    const settings = await readRegistrySettings(path);
    scoped ??= settings["@cortexkit:registry"];
    registry ??= settings.registry;
    if (dirname(current) === current) break;
  }
  const explicitUserConfig = process.env.npm_config_userconfig ?? process.env.NPM_CONFIG_USERCONFIG;
  const home =
    (process.platform === "win32" ? process.env.USERPROFILE : process.env.HOME) ?? homedir();
  const userConfig = explicitUserConfig ?? join(home, ".npmrc");
  if (!scoped && !visited.has(userConfig)) {
    const settings = await readRegistrySettings(userConfig, explicitUserConfig !== undefined);
    scoped = settings["@cortexkit:registry"];
    registry ??= settings.registry;
  }
  const selected = scoped ?? registry ?? NPM_REGISTRY_URL;
  return selected.replace(/\$\{([^}]+)\}/g, (_match, name: string) => {
    const value = process.env[name];
    if (value === undefined) throw new Error("Unresolved registry environment variable");
    return value;
  });
}

function validateRegistry(registry: string): string {
  const url = new URL(registry);
  if (url.protocol !== "https:" && url.protocol !== "http:")
    throw new Error("Unsupported registry protocol");
  return registry;
}

const reportedRegistryFailures = new Set<string>();

function reportRegistryFailure(directory: string): void {
  const key = resolve(directory);
  if (reportedRegistryFailures.has(key)) return;
  reportedRegistryFailures.add(key);
  // Do not log registry URLs or parser errors: either can contain credentials.
  debug(
    "[auto-update-checker] Configured npm registry could not be resolved or used; skipping update check.",
  );
}

function resolveRegistry(directory: string): Promise<string> {
  const key = resolve(directory);
  let pending = registryCache.get(key);
  if (!pending) {
    // npm applies full config precedence; bun-only hosts use asynchronous npmrc reads.
    // Cache both outcomes so an unreadable private config never causes a public query.
    pending = (async () => {
      let registry: string;
      try {
        const scoped = configuredRegistry(await npmConfigGet("@cortexkit:registry", key));
        registry =
          scoped ?? configuredRegistry(await npmConfigGet("registry", key)) ?? NPM_REGISTRY_URL;
      } catch {
        registry = await registryWithoutNpm(key);
      }
      return validateRegistry(registry);
    })();
    registryCache.set(key, pending);
  }
  return pending;
}

function buildRegistryUrl(registryUrl: string): string {
  return `${registryUrl.replace(/\/+$/, "")}/${encodeURIComponent(PACKAGE_NAME).replace("%2F", "/")}`;
}

export async function getLatestVersion(
  channel = "latest",
  options: {
    registryUrl?: string;
    directory?: string;
    timeoutMs?: number;
    signal?: AbortSignal;
  } = {},
): Promise<string | null> {
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), options.timeoutMs ?? NPM_FETCH_TIMEOUT);
  const abortHandler = () => controller.abort();
  options.signal?.addEventListener("abort", abortHandler, { once: true });

  try {
    if (options.signal?.aborted) return null;
    const registry = validateRegistry(
      options.registryUrl ?? (await resolveRegistry(options.directory ?? process.cwd())),
    );
    if (controller.signal.aborted) return null;
    const response = await fetch(buildRegistryUrl(registry), {
      signal: controller.signal,
      headers: { Accept: "application/json" },
    });
    if (!response.ok) {
      reportRegistryFailure(options.directory ?? process.cwd());
      return null;
    }

    const data = NpmPackageEnvelopeSchema.safeParse(await response.json());
    if (!data.success) return null;
    return data.data["dist-tags"][channel] ?? data.data["dist-tags"].latest ?? null;
  } catch {
    if (!controller.signal.aborted) reportRegistryFailure(options.directory ?? process.cwd());
    return null;
  } finally {
    options.signal?.removeEventListener("abort", abortHandler);
    clearTimeout(timeoutId);
  }
}
