/**
 * Resolve an `npm` executable when it is not on PATH.
 *
 * OpenCode and Pi are frequently launched from a GUI / dock / Desktop app,
 * which gives the process a stripped PATH that does NOT include a Node version
 * manager's bin directory (nvm, mise, volta, fnm, asdf) or even Homebrew. When
 * that happens, `spawn("npm", ...)` fails with "Executable not found in $PATH",
 * so the auto-updater and LSP auto-install silently break. See issue: a user's
 * auto-update churned every launch (rewrite package.json -> delete package ->
 * npm install fails -> restore) and they stayed pinned to the old version.
 *
 * `npm` is itself a Node script (`#!/usr/bin/env node` shebang on Unix), so once
 * we find npm's absolute path we must also make its sibling `node` reachable, or
 * the shebang fails the same way. `resolveNpm()` returns both the command and
 * its bin directory; `npmSpawnEnv()` prepends that directory to PATH for the
 * spawn so npm can find its own node.
 */
import { execFileSync } from "node:child_process";
import { readdirSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { delimiter, dirname, isAbsolute, join } from "node:path";

export interface ResolvedNpm {
  /** Absolute path to the npm executable (or a bare name if only PATH-resolved). */
  command: string;
  /** Directory containing npm, prepended to PATH at spawn time so npm's
   * `#!/usr/bin/env node` shebang can find its sibling node. Null when the
   * command was found via the OS PATH resolver and no augmentation is needed. */
  binDir: string | null;
}

interface ResolveNpmDeps {
  platform: NodeJS.Platform;
  env: NodeJS.ProcessEnv;
  home: string;
  execPath: string;
  /**
   * Absolute system bin directories scanned last (e.g. /usr/local/bin). Defaults
   * to the platform's well-known list. Injectable so tests can pass `[]` to stay
   * hermetic — otherwise a real system npm (present on CI runners) leaks in and
   * breaks the "returns null" cases.
   */
  systemNpmDirs?: string[];
}

function defaultDeps(): ResolveNpmDeps {
  return {
    platform: process.platform,
    env: process.env,
    home: homedir(),
    execPath: process.execPath,
  };
}

function npmBinaryName(platform: NodeJS.Platform): string {
  return platform === "win32" ? "npm.cmd" : "npm";
}

function isFile(p: string): boolean {
  try {
    return statSync(p).isFile();
  } catch {
    return false;
  }
}

/** Scan the PATH env for npm. Returns the first match's directory, or null. */
function npmFromPath(deps: ResolveNpmDeps): string | null {
  const name = npmBinaryName(deps.platform);
  const raw = deps.env.PATH ?? deps.env.Path ?? "";
  for (const entry of raw.split(delimiter)) {
    const dir = entry.trim().replace(/^"|"$/g, "");
    if (!dir || !isAbsolute(dir)) continue;
    if (isFile(join(dir, name))) return dir;
  }
  return null;
}

/** npm ships beside node in standard installs (e.g. /opt/homebrew/bin/{node,npm}). */
function npmAdjacentToNode(deps: ResolveNpmDeps): string | null {
  // process.execPath is the running node/bun binary. Under Node this is
  // .../bin/node with npm as a sibling; under Bun (OpenCode TUI) there is no
  // npm sibling, which is fine — we fall through to well-known locations.
  const dir = dirname(deps.execPath);
  return isFile(join(dir, npmBinaryName(deps.platform))) ? dir : null;
}

/**
 * Pick the highest-version subdirectory under a version-manager `installs`
 * directory that actually contains npm. Used for nvm / mise layouts like
 * `~/.nvm/versions/node/<ver>/bin/npm`.
 */
function highestVersionedNodeBin(installsDir: string, name: string): string | null {
  let entries: string[];
  try {
    entries = readdirSync(installsDir);
  } catch {
    return null;
  }
  const candidates = entries
    .filter((v) => isFile(join(installsDir, v, "bin", name)))
    .sort((a, b) => compareVersionsDesc(a, b));
  return candidates.length > 0 ? join(installsDir, candidates[0], "bin") : null;
}

/** Descending semver-ish compare; non-numeric segments sort after numeric. */
function compareVersionsDesc(a: string, b: string): number {
  const pa = a
    .replace(/^v/, "")
    .split(".")
    .map((n) => Number.parseInt(n, 10));
  const pb = b
    .replace(/^v/, "")
    .split(".")
    .map((n) => Number.parseInt(n, 10));
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const na = Number.isFinite(pa[i]) ? pa[i] : -1;
    const nb = Number.isFinite(pb[i]) ? pb[i] : -1;
    if (na !== nb) return nb - na;
  }
  return b.localeCompare(a);
}

/** Well-known npm bin directories, in priority order, for the current platform. */
function wellKnownNpmDirs(deps: ResolveNpmDeps): string[] {
  const { platform, env, home } = deps;
  const name = npmBinaryName(platform);
  const dirs: string[] = [];
  const push = (dir: string | null | undefined) => {
    if (dir && !dirs.includes(dir)) dirs.push(dir);
  };

  if (platform === "win32") {
    const programFiles = env.ProgramFiles || "C:\\Program Files";
    const appData = env.APPDATA;
    const localAppData = env.LOCALAPPDATA;
    push(join(programFiles, "nodejs"));
    if (appData) push(join(appData, "npm"));
    if (localAppData) push(join(localAppData, "Volta", "bin"));
    // nvm-windows
    if (env.NVM_SYMLINK) push(env.NVM_SYMLINK);
  } else {
    // Active node version manager hints (set even when PATH is otherwise stripped).
    if (env.NVM_BIN) push(env.NVM_BIN);
    // Version-manager installs (pick highest version with npm).
    push(highestVersionedNodeBin(join(home, ".nvm", "versions", "node"), name));
    push(highestVersionedNodeBin(join(home, ".local", "share", "mise", "installs", "node"), name));
    push(highestVersionedNodeBin(join(home, ".asdf", "installs", "nodejs"), name));
    // Fixed-location managers.
    push(join(home, ".volta", "bin"));
    push(join(home, ".asdf", "shims"));
    // Homebrew + system (injectable so tests stay hermetic; see ResolveNpmDeps).
    const systemDirs =
      deps.systemNpmDirs ??
      (platform === "darwin"
        ? ["/opt/homebrew/bin", "/usr/local/bin"]
        : ["/usr/local/bin", "/usr/bin", join(home, ".local", "bin")]);
    for (const dir of systemDirs) push(dir);
  }
  return dirs;
}

/**
 * Resolve npm, preferring PATH, then node-adjacent, then well-known version
 * manager / system locations. Returns null only when npm genuinely cannot be
 * found anywhere we know to look.
 */
export function resolveNpm(deps: ResolveNpmDeps = defaultDeps()): ResolvedNpm | null {
  const name = npmBinaryName(deps.platform);

  // 1. PATH — respects the user's own setup when it survived to this process.
  const onPath = npmFromPath(deps);
  if (onPath) return { command: join(onPath, name), binDir: onPath };

  // 2. Node-adjacent (npm sits next to node in standard installs).
  const adjacent = npmAdjacentToNode(deps);
  if (adjacent) return { command: join(adjacent, name), binDir: adjacent };

  // 3. Well-known version-manager / system locations.
  for (const dir of wellKnownNpmDirs(deps)) {
    const candidate = join(dir, name);
    if (isFile(candidate)) return { command: candidate, binDir: dir };
  }

  return null;
}

/**
 * Build a spawn env that makes a resolved npm runnable: prepend its bin dir to
 * PATH so npm's `#!/usr/bin/env node` shebang finds its sibling node, even when
 * the inherited PATH was stripped by a GUI launch.
 */
export function npmSpawnEnv(
  resolved: ResolvedNpm,
  baseEnv: NodeJS.ProcessEnv = process.env,
): NodeJS.ProcessEnv {
  if (!resolved.binDir) return { ...baseEnv };
  const existing = baseEnv.PATH ?? baseEnv.Path ?? "";
  const next = existing ? `${resolved.binDir}${delimiter}${existing}` : resolved.binDir;
  return { ...baseEnv, PATH: next };
}

/**
 * Quick boolean check: can we run npm at all? Used by pre-flight gating before
 * destructive auto-update steps.
 */
export function isNpmAvailable(deps: ResolveNpmDeps = defaultDeps()): boolean {
  return resolveNpm(deps) !== null;
}

/** Test seam: verify a resolved npm actually executes (used by diagnostics). */
export function probeNpmVersion(resolved: ResolvedNpm): string | null {
  try {
    const out = execFileSync(resolved.command, ["--version"], {
      env: npmSpawnEnv(resolved),
      encoding: "utf-8",
      timeout: 5000,
      stdio: ["ignore", "pipe", "ignore"],
    });
    const v = out.trim();
    return /^\d+\.\d+\.\d+/.test(v) ? v : null;
  } catch {
    return null;
  }
}
