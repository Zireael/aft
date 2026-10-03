/**
 * With the real bridge pool and a real aft binary: a host that loads the
 * extension with no session must still exit on its own, with no aft process
 * left behind.
 *
 * GitHub issue #389: `omp plugin upgrade @cortexkit/aft-pi` printed
 * "Installed ..." and then never returned. OMP validates an installed plugin
 * by calling its extension factory with no session, and the factory's eager
 * warmup spawned an aft bridge whose pipes kept OMP's event loop alive.
 * fixtures/real-bridge-exit-harness.ts loads the extension that way in a child
 * process; a second test runs a session through the same harness to show the
 * bridge still starts at session_start and still goes away with the session.
 */

import { afterAll, describe, expect, test } from "bun:test";
import { spawn, spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { prepareBinary } from "./helpers.js";

/** How long the process may live after the harness emitted its done event. */
const EXIT_BOUND_MS = 5_000;
/** When to stop waiting and kill a child that is evidently not going to exit. */
const HANG_KILL_MS = 15_000;

const tempDirs: string[] = [];

/** Pids of running processes whose command line contains `needle`. */
function processesMatching(needle: string): number[] {
  const result = spawnSync("pgrep", ["-f", needle], { encoding: "utf8" });
  return (result.stdout ?? "")
    .split("\n")
    .map((line) => Number.parseInt(line.trim(), 10))
    .filter((pid) => Number.isInteger(pid));
}

/** Wait up to `ms` for every process matching `needle` to be gone. */
async function processesLeftAfter(needle: string, ms: number): Promise<number[]> {
  const deadline = Date.now() + ms;
  let pids = processesMatching(needle);
  while (pids.length > 0 && Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 100));
    pids = processesMatching(needle);
  }
  return pids;
}

interface RealRun {
  events: Array<{ name: string; detail: string }>;
  exitCode: number | null;
  /**
   * Time from the harness's done event (`validate-done` or `shutdown-done`) to
   * process exit, unless it was killed.
   */
  exitedAfterDoneMs: number | null;
  killedAsHung: boolean;
  /** aft processes from this run still alive shortly after the host exited. */
  leftoverAft: number[];
  stderr: string;
}

function runRealHarness(
  realBinary: string,
  mode: "plugin-validate" | "session",
  doneEvent: string,
): Promise<RealRun> {
  const tempDir = mkdtempSync(join(tmpdir(), "aft-pi-no-session-exit-"));
  tempDirs.push(tempDir);
  const binDir = join(tempDir, "bin");
  const projectDir = join(tempDir, "project");
  const configDir = join(tempDir, "config");
  mkdirSync(binDir, { recursive: true });
  mkdirSync(projectDir, { recursive: true });
  mkdirSync(join(configDir, "cortexkit"), { recursive: true });
  // Spawn aft under a path unique to this run, so the processes it leaves (or
  // not) can be told apart from every other aft on the machine.
  const binaryPath = join(binDir, "aft");
  symlinkSync(realBinary, binaryPath);
  writeFileSync(join(projectDir, "a.ts"), "export const a = 1;\n");
  // No network: no LSP installs and no ONNX Runtime download.
  writeFileSync(
    join(configDir, "cortexkit", "aft.jsonc"),
    '{ "lsp": { "auto_install": false }, "indexes": { "semantic": false } }\n',
  );

  const env: Record<string, string> = {};
  for (const [key, value] of Object.entries(process.env)) {
    if (value !== undefined) env[key] = value;
  }
  delete env.AFT_CACHE_DIR;
  delete env.MAGIC_CONTEXT_PI_SUBAGENT;
  Object.assign(env, {
    HOME: join(tempDir, "home"),
    XDG_CONFIG_HOME: configDir,
    XDG_CACHE_HOME: join(tempDir, "cache"),
    XDG_DATA_HOME: join(tempDir, "data"),
    XDG_STATE_HOME: join(tempDir, "state"),
    AFT_BINARY_PATH: binaryPath,
    HARNESS_BINARY: binaryPath,
    HARNESS_PLUGIN: resolve(import.meta.dir, "../../index.ts"),
    HARNESS_MODE: mode,
  });

  return new Promise<RealRun>((resolveRun, rejectRun) => {
    const child = spawn(
      process.execPath,
      ["run", resolve(import.meta.dir, "../fixtures/real-bridge-exit-harness.ts")],
      { cwd: projectDir, env, stdio: ["ignore", "pipe", "pipe"] },
    );
    const run: RealRun = {
      events: [],
      exitCode: null,
      exitedAfterDoneMs: null,
      killedAsHung: false,
      leftoverAft: [],
      stderr: "",
    };
    let doneAt: number | null = null;
    let hangTimer: ReturnType<typeof setTimeout> | undefined;
    let stdoutBuf = "";
    child.stdout.on("data", (chunk) => {
      stdoutBuf += String(chunk);
      let newline = stdoutBuf.indexOf("\n");
      while (newline >= 0) {
        const line = stdoutBuf.slice(0, newline);
        stdoutBuf = stdoutBuf.slice(newline + 1);
        newline = stdoutBuf.indexOf("\n");
        const match = /^EVENT (\S+) ?(.*)$/.exec(line);
        if (!match) continue;
        run.events.push({ name: match[1] ?? "", detail: match[2] ?? "" });
        if (match[1] === doneEvent) {
          doneAt = Date.now();
          hangTimer = setTimeout(() => {
            run.killedAsHung = true;
            child.kill("SIGKILL");
          }, HANG_KILL_MS);
        }
      }
    });
    child.stderr.on("data", (chunk) => {
      run.stderr = (run.stderr + String(chunk)).slice(-4_000);
    });
    const overallTimer = setTimeout(() => {
      run.killedAsHung = true;
      child.kill("SIGKILL");
    }, 60_000);
    child.on("error", rejectRun);
    child.on("exit", async (code) => {
      clearTimeout(overallTimer);
      clearTimeout(hangTimer);
      run.exitCode = code;
      if (doneAt !== null && !run.killedAsHung) run.exitedAfterDoneMs = Date.now() - doneAt;
      // A bridge whose host was killed sees its stdin close and exits on its
      // own; give it a moment before calling it left behind.
      run.leftoverAft = await processesLeftAfter(binaryPath, 3_000);
      for (const pid of run.leftoverAft) {
        try {
          process.kill(pid, "SIGKILL");
        } catch {
          // Already gone.
        }
      }
      resolveRun(run);
    });
  });
}

afterAll(() => {
  for (const dir of tempDirs) rmSync(dir, { recursive: true, force: true });
}, 30_000);

describe.skipIf(process.platform === "win32")("Pi extension with a real aft binary", () => {
  test("a load with no session (omp plugin install / upgrade) exits on its own and spawns no aft", async () => {
    const prep = await prepareBinary();
    if (!prep.binaryPath) return;
    const run = await runRealHarness(prep.binaryPath, "plugin-validate", "validate-done");
    if (run.killedAsHung || run.exitCode !== 0) console.error(`harness stderr:\n${run.stderr}`);
    expect(run.events.find((event) => event.name === "validate-done")?.detail).toBe("aft=");
    expect(run.killedAsHung).toBe(false);
    expect(run.exitCode).toBe(0);
    expect(run.exitedAfterDoneMs ?? Number.POSITIVE_INFINITY).toBeLessThan(EXIT_BOUND_MS);
    expect(run.leftoverAft).toEqual([]);
  }, 90_000);

  test("a session starts the aft bridge at session_start and the process exits after session_shutdown", async () => {
    const prep = await prepareBinary();
    if (!prep.binaryPath) return;
    const run = await runRealHarness(prep.binaryPath, "session", "shutdown-done");
    if (run.killedAsHung || run.exitCode !== 0) console.error(`harness stderr:\n${run.stderr}`);
    expect(run.events.find((event) => event.name === "bridge-alive")?.detail).toMatch(/^\d+/);
    expect(run.killedAsHung).toBe(false);
    expect(run.exitCode).toBe(0);
    expect(run.exitedAfterDoneMs ?? Number.POSITIVE_INFINITY).toBeLessThan(EXIT_BOUND_MS);
    expect(run.leftoverAft).toEqual([]);
  }, 90_000);
});
