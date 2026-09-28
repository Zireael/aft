import { describe, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { resolve } from "node:path";

const rig = resolve(import.meta.dir, "e2e/subc-rig.ts");

function prepare(env: Record<string, string>) {
  return spawnSync(
    process.execPath,
    [
      "--eval",
      `const { prepareSubcLane } = await import(${JSON.stringify(rig)}); await prepareSubcLane();`,
    ],
    {
      env: { ...process.env, CI: "false", SUBC_CORE_WIRE_VERSION: "", ...env },
      encoding: "utf8",
      timeout: 15_000,
    },
  );
}

describe("subc e2e preflight", () => {
  test("an explicitly requested missing daemon fails rather than skipping", () => {
    const result = prepare({ SUBC_CORE_BIN: "/nonexistent/aft-e2e-subc-core" });
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("e2e setup failed: SUBC_CORE_BIN is not executable");
  });

  test("an explicitly requested daemon without a wire version fails rather than skipping", () => {
    const result = prepare({ SUBC_CORE_BIN: process.execPath });
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("wire version unknown");
    expect(result.stderr).toContain("SUBC_CORE_WIRE_VERSION=2");
  });
});
