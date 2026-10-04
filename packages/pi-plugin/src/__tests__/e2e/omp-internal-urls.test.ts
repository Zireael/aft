/// <reference path="../../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const host = process.env.AFT_OMP_HOST;
if (!host)
  console.warn(
    "[omp-internal-urls] skipped: set AFT_OMP_HOST to a directory with OMP 18.6.x installed",
  );

describe.skipIf(!host)("OMP 18.6.x headless internal URLs", () => {
  test("AFT tool surface preserves agent reads, content-less proc kill and skills in an isolated HOME", async () => {
    const home = mkdtempSync(join(tmpdir(), "aft-omp-urls-"));
    try {
      mkdirSync(join(home, "project"));
      const env = {
        ...process.env,
        HOME: home,
        XDG_CONFIG_HOME: join(home, "config"),
        XDG_DATA_HOME: join(home, "data"),
        XDG_CACHE_HOME: join(home, "cache"),
        XDG_STATE_HOME: join(home, "state"),
        AFT_OMP_HOST: resolve(host!),
      };
      const child = Bun.spawn(
        [process.execPath, resolve(import.meta.dir, "../fixtures/omp-internal-urls-harness.ts")],
        { env, cwd: join(home, "project"), stdout: "pipe", stderr: "pipe", timeout: 45_000 },
      );
      const [exit, stdout, stderr] = await Promise.all([
        child.exited,
        new Response(child.stdout).text(),
        new Response(child.stderr).text(),
      ]);
      expect(exit, `${stdout}\n${stderr}`).toBe(0);
      const evidence = JSON.parse(stdout.trim().split("\n").at(-1)!);
      expect(evidence.version).toMatch(/^OMP 18\.6\./);
      expect(evidence.checks).toBe(5);
      expect(evidence.registeredMcp).toBe(true);
      expect(evidence.skillPrompt).toBe(true);
    } finally {
      rmSync(home, { recursive: true, force: true });
    }
  }, 60_000);
});
