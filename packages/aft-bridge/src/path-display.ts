import { homedir } from "node:os";
import { posix, win32 } from "node:path";

/** Shorten only the home directory itself and its descendants, not sibling prefixes. */
export function shortenHomePath(
  path: string,
  home = homedir(),
  platform: NodeJS.Platform = process.platform,
): string {
  if (!home) return path;
  const normalize = (value: string) =>
    platform === "win32" ? value.replace(/\\/g, "/").toLowerCase() : value;
  const base = normalize(home).replace(/\/+$/, "");
  const target = normalize(path);
  if (target === normalize(home) || target === base) return "~";
  return target.startsWith(`${base}/`) ? `~${path.slice(base.length)}` : path;
}

/** A relative path escapes its root only through a parent segment or an absolute path. */
export function relativePathEscapesRoot(
  rel: string,
  platform: NodeJS.Platform = process.platform,
): boolean {
  return (
    rel === ".." ||
    rel.startsWith("../") ||
    (platform === "win32" && rel.startsWith("..\\")) ||
    (platform === "win32" ? win32.isAbsolute(rel) : posix.isAbsolute(rel))
  );
}
