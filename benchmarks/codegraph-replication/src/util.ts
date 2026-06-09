import { existsSync, readdirSync, statSync } from "node:fs";
import { dirname, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const SRC_DIR = dirname(fileURLToPath(import.meta.url));
export const HARNESS_DIR = resolve(SRC_DIR, "..");
export const REPO_ROOT = resolve(HARNESS_DIR, "../..");

export function roundMetric(value: number, digits = 6): number {
  return Number.isFinite(value) ? Number(value.toFixed(digits)) : 0;
}

export function percentile(values: number[], pct: number): number {
  if (values.length === 0) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  if (sorted.length === 1) return roundMetric(sorted[0], 3);
  const index = Math.max(
    0,
    Math.min(sorted.length - 1, Math.ceil((pct / 100) * sorted.length) - 1),
  );
  return roundMetric(sorted[index], 3);
}

export function median(values: number[]): number {
  return percentile(values, 50);
}

export function normalizePath(raw: string | undefined, codebasePath: string): string | undefined {
  if (!raw) return undefined;
  const cleaned = raw.replace(/\\/g, "/");
  const absolute = cleaned.startsWith("/") ? cleaned : resolve(codebasePath, cleaned);
  try {
    return relative(codebasePath, absolute).replace(/\\/g, "/") || ".";
  } catch {
    return cleaned;
  }
}

export function escapeRegexLiteral(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

export function unique<T>(values: T[]): T[] {
  return [...new Set(values)];
}

export async function runCommand(
  command: string[],
  cwd: string,
  timeoutMs: number,
): Promise<{ stdout: string; stderr: string; exitCode: number; timedOut: boolean }> {
  const proc = Bun.spawn(command, {
    cwd,
    stdout: "pipe",
    stderr: "pipe",
  });
  let timedOut = false;
  const timer = setTimeout(() => {
    timedOut = true;
    proc.kill();
  }, timeoutMs);
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ]);
  clearTimeout(timer);
  return { stdout, stderr, exitCode, timedOut };
}

export function gitRev(path: string, short = false): string {
  const args = ["git", "-C", path, "rev-parse", short ? "--short" : "HEAD"];
  const proc = Bun.spawnSync(args, { stdout: "pipe", stderr: "pipe" });
  if (proc.exitCode !== 0) return "unknown";
  return new TextDecoder().decode(proc.stdout).trim() || "unknown";
}

export function listProjectFiles(root: string, limit: number): string[] {
  const out: string[] = [];
  const ignored = new Set([".git", "node_modules", "target", "dist", ".cache"]);

  function walk(dir: string): void {
    if (out.length >= limit) return;
    let entries: string[];
    try {
      entries = readdirSync(dir).sort();
    } catch {
      return;
    }
    for (const entry of entries) {
      if (out.length >= limit) return;
      if (ignored.has(entry)) continue;
      const path = resolve(dir, entry);
      let stat: ReturnType<typeof statSync>;
      try {
        stat = statSync(path);
      } catch {
        continue;
      }
      if (stat.isDirectory()) {
        walk(path);
      } else if (stat.isFile()) {
        out.push(relative(root, path).replace(/\\/g, "/"));
      }
    }
  }

  if (existsSync(root)) walk(root);
  return out;
}
