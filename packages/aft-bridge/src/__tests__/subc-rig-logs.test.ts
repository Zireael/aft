import { expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { mkdtemp, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

test.skipIf(process.platform === "win32")(
  "subc rig retains module and daemon stderr across failed startup retries",
  async () => {
    const dir = await mkdtemp(join(tmpdir(), "subc-log-test-"));
    try {
      const daemon = join(dir, "daemon");
      const module = join(dir, "module");
      const logs = join(dir, "logs");
      await writeFile(module, '#!/bin/sh\nprintf "module panic diagnostic\\n" >&2\n', {
        mode: 0o700,
      });
      await writeFile(
        daemon,
        `#!${process.execPath}
import { readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
const config = JSON.parse(readFileSync(process.env.XDG_CONFIG_HOME + "/cortexkit/subc.jsonc", "utf8"));
const mod = config.modules.aft;
spawnSync(mod.program, mod.args, { env: { ...process.env, ...mod.env }, stdio: "inherit" });
console.error("daemon startup diagnostic");
process.exit(1);
`,
        { mode: 0o700 },
      );
      const rig = resolve(import.meta.dir, "e2e/subc-rig.ts");
      const result = spawnSync(
        process.execPath,
        [
          "--eval",
          `
const { startSubcRig } = await import(${JSON.stringify(rig)});
try {
  await startSubcRig({ aftBinaryPath: ${JSON.stringify(module)}, subcCorePath: ${JSON.stringify(daemon)}, buildAttempted: false });
  process.exit(2);
} catch { process.exit(0); }
`,
        ],
        { env: { ...process.env, AFT_SUBC_E2E_LOG_DIR: logs }, encoding: "utf8", timeout: 10_000 },
      );
      expect(result.status).toBe(0);
      const entries = await readdir(logs);
      expect(entries).toHaveLength(1);
      const logDir = join(logs, entries[0]!);
      expect(await readFile(join(logDir, "aft.stderr.log"), "utf8")).toBe(
        "module panic diagnostic\n".repeat(2),
      );
      expect(await readFile(join(logDir, "subc-core.stderr.log"), "utf8")).toBe(
        "daemon startup diagnostic\n".repeat(2),
      );
      expect(result.stdout).toContain(logDir);
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  },
  15_000,
);
