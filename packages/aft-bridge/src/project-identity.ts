import { createHash } from "node:crypto";
import { type FSWatcher, realpathSync, statSync, watch } from "node:fs";
import { basename, dirname, resolve } from "node:path";

const rootMemo = new Map<string, { canonical: string; stamp: string; watchers: FSWatcher[] }>();
const rootWork = { realpaths: 0, stats: 0 };
export function __projectRootWorkForTests(): typeof rootWork {
  return { ...rootWork };
}

function rootStamp(path: string): string {
  rootWork.stats += 1;
  const s = statSync(path, { bigint: true });
  return `${s.dev}:${s.ino}:${s.ctimeNs}`;
}

export function invalidateProjectRootMemo(dir: string): void {
  const key = resolve(dir);
  const entry = rootMemo.get(key);
  if (!entry) return;
  rootMemo.delete(key);
  for (const watcher of entry.watchers) watcher.close();
}

// Watch aliases AND canonical ancestors: renaming an ancestor can change the
// absolute path without changing the descendant directory's own stat stamp.
function watchRootAncestors(key: string, canonical: string): FSWatcher[] {
  const watchers: FSWatcher[] = [];
  const seen = new Set<string>();
  try {
    for (const path of new Set([key, canonical])) {
      let child = path;
      while (dirname(child) !== child) {
        const parent = dirname(child);
        const watchKey = `${parent}\u0000${basename(child)}`;
        if (!seen.has(watchKey)) {
          seen.add(watchKey);
          const name = basename(child);
          const watcher = watch(parent, { persistent: false }, (_event, filename) => {
            if (filename === null || filename.toString() === name) invalidateProjectRootMemo(key);
          });
          watcher.on("error", () => invalidateProjectRootMemo(key));
          watchers.push(watcher);
        }
        child = parent;
      }
    }
  } catch {
    for (const watcher of watchers) watcher.close();
    return [];
  }
  return watchers;
}

/**
 * The single TypeScript project-root canonicalizer, mirroring the Rust
 * `cortexkit-paths` `ProjectRootId`: resolve symlinks (`realpath`), strip
 * trailing separators, normalize Windows verbatim/UNC prefixes, and uppercase
 * the drive letter. Falls back to lexical resolution for paths that don't
 * exist (so callers that canonicalize a not-yet-created or transient path stay
 * total instead of throwing).
 *
 * Why one canonicalizer: AFT used to derive project-root identity four
 * different ways across the TS layer — bridge routing realpath'd, but RPC
 * port-file scoping (`projectHash`) and the sidebar status gate compared raw
 * strings. That divergence is the bug behind sidebar-shows-wrong-project and
 * stale-port discovery: a symlinked / raw-spelled launch dir hashed to a
 * different port directory than the bridge routed to.
 *
 * TS↔Rust *pixel* parity is NOT required (under the daemon, subc and AFT
 * re-canonicalize the received root authoritatively). What IS required is
 * TS-internal self-consistency: every routing, scoping, and port-file site
 * routes through THIS function, so the bridge routing key and the RPC port
 * scope always agree for the same project.
 */
export function canonicalizeProjectRoot(dir: string): string {
  const trimmed = dir.replace(/[/\\]+$/, "");
  const key = resolve(trimmed);
  let canonical: string;
  let stamp: string | undefined;
  try {
    stamp = rootStamp(key);
    const known = rootMemo.get(key);
    if (known && known.stamp === stamp) {
      try {
        if (key === known.canonical || rootStamp(known.canonical) === stamp) return known.canonical;
      } catch {
        // A renamed target must be realpathed again, not treated as a missing alias.
      }
    }
    invalidateProjectRootMemo(key);
    rootWork.realpaths += 1;
    canonical = realpathSync(trimmed);
  } catch {
    invalidateProjectRootMemo(key);
    canonical = resolve(trimmed);
    stamp = undefined;
  }
  canonical = normalizeWindowsRoot(canonical);
  if (stamp !== undefined) {
    const watchers = watchRootAncestors(key, canonical);
    if (watchers.length > 0) {
      if (rootMemo.size >= 128) invalidateProjectRootMemo(rootMemo.keys().next().value as string);
      rootMemo.set(key, { canonical, stamp, watchers });
    }
  }
  return canonical;
}

/**
 * Strip only safely convertible Windows extended-length DOS and UNC prefixes,
 * and uppercase a lowercase drive letter so `c:\x` and `C:\x` collapse to one
 * identity. Namespaces such as `\\?\Volume{GUID}\` must retain their prefix.
 * `platform` is injectable for cross-platform regression tests. No-op off Windows.
 */
export function normalizeWindowsRoot(p: string, platform = process.platform): string {
  if (platform !== "win32") return p;
  let s = p;
  const lower = s.toLowerCase();
  const uncPrefix = ["\\\\?\\unc\\", "\\\\??\\unc\\", "\\??\\unc\\"].find((prefix) =>
    lower.startsWith(prefix),
  );
  if (uncPrefix) {
    const tail = s.slice(uncPrefix.length);
    if (/^[^\\/]+[\\/][^\\/]+(?:[\\/]|$)/.test(tail) && !hasDotComponent(tail)) {
      s = `\\\\${tail}`;
    }
  } else {
    const dosPrefix = ["\\\\?\\", "\\\\??\\", "\\??\\"].find((prefix) => lower.startsWith(prefix));
    if (dosPrefix) {
      const tail = s.slice(dosPrefix.length);
      if (/^[a-z]:[\\/]/i.test(tail) && !hasDotComponent(tail)) s = tail;
    }
  }
  if (s.length >= 2 && s[1] === ":") {
    const drive = s.charCodeAt(0);
    if (drive >= 97 && drive <= 122) {
      s = s[0].toUpperCase() + s.slice(1);
    }
  }
  return s;
}

function hasDotComponent(path: string): boolean {
  return path.split(/[\\/]/).some((component) => component === "." || component === "..");
}

/**
 * Stable 16-hex scope hash of the canonical project root. Used for RPC
 * port-file directory scoping; because it canonicalizes first, the server
 * writing a port file and the client discovering it agree on the directory
 * even when one was handed a symlinked or raw-spelled path.
 */
export function projectRootKeyHash(dir: string): string {
  return createHash("sha256").update(canonicalizeProjectRoot(dir)).digest("hex").slice(0, 16);
}
