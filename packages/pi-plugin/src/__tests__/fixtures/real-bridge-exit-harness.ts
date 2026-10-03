/**
 * Child-process harness for e2e/no-session-exit.test.ts. Run it with
 * `bun run`; it is not a test file itself.
 *
 * Loads the real Pi extension with the real bridge pool and a real aft binary
 * (AFT_BINARY_PATH), with no module replaced, the way a host does.
 *
 * Environment (set by the parent test):
 *   HARNESS_PLUGIN  absolute path of the plugin entry (src/index.ts)
 *   HARNESS_BINARY  path the aft binary is spawned under, unique to this run
 *   HARNESS_MODE    "plugin-validate": call the factory and fire no session
 *                   event, the way `omp plugin install` / `omp plugin upgrade`
 *                   validate an extension, then let the script end;
 *                   "session": fire session_start, wait until the warmup aft
 *                   process runs, then fire session_shutdown
 *
 * Stdout protocol, one line each: `EVENT <name> <detail>`.
 */

import { spawnSync } from "node:child_process";

function emit(name: string, detail = ""): void {
  process.stdout.write(`EVENT ${name} ${detail}\n`);
}

/** Pids of running processes whose command line contains `needle`. */
function processesMatching(needle: string): number[] {
  const result = spawnSync("pgrep", ["-f", needle], { encoding: "utf8" });
  return (result.stdout ?? "")
    .split("\n")
    .map((line) => Number.parseInt(line.trim(), 10))
    .filter((pid) => Number.isInteger(pid) && pid !== process.pid);
}

const pluginPath = process.env.HARNESS_PLUGIN;
const binaryPath = process.env.HARNESS_BINARY;
const mode = process.env.HARNESS_MODE;
if (!pluginPath || !binaryPath || !mode) throw new Error("harness env missing");

const handlers = new Map<string, Array<(...args: unknown[]) => unknown>>();
const pi = {
  registerTool() {},
  registerCommand() {},
  on(event: string, handler: (...args: unknown[]) => unknown) {
    const list = handlers.get(event) ?? [];
    list.push(handler);
    handlers.set(event, list);
  },
};

const plugin = (await import(pluginPath)).default as (api: unknown) => Promise<void>;
await plugin(pi);
emit("plugin-ready");

if (mode === "plugin-validate") {
  // OMP's PluginManager.install() calls loadExtensions() on the installed entry
  // and returns; the CLI prints "Installed ..." and its main() returns. The
  // parent measures how long this process lives after this line.
  emit("validate-done", `aft=${processesMatching(binaryPath).join(",")}`);
} else {
  const extCtx = {
    cwd: process.cwd(),
    hasUI: false,
    sessionManager: { getSessionId: () => "e2e-session" },
  };
  for (const handler of handlers.get("session_start") ?? []) {
    await handler({ type: "session_start", reason: "startup" }, extCtx);
  }
  const deadline = Date.now() + 20_000;
  let pids = processesMatching(binaryPath);
  while (pids.length === 0 && Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 50));
    pids = processesMatching(binaryPath);
  }
  emit("bridge-alive", pids.join(","));
  for (const handler of handlers.get("session_shutdown") ?? []) {
    await handler({ type: "session_shutdown" }, extCtx);
  }
  emit("shutdown-done");
}
