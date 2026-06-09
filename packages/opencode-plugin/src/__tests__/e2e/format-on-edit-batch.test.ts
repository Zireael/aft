/// <reference path="../../bun-test.d.ts" />

import { afterEach, beforeAll, describe, expect, test } from "bun:test";
import { mkdir, writeFile } from "node:fs/promises";
import type { BridgePool } from "@cortexkit/aft-bridge";
import {
  BIOME_TS_EXCLUDED_PRESET,
  BIOME_TS_PRESET,
  biomeExcludedPathShim,
  createFormatHarness,
  tsCollapseSpacesShim,
} from "./format-helpers.js";
import {
  cleanupHarnesses,
  type E2EHarness,
  type PreparedBinary,
  prepareBinary,
  readTextFile,
} from "./helpers.js";

const initialBinary = await prepareBinary();
const maybeDescribe = describe.skipIf(!initialBinary.binaryPath);

maybeDescribe("e2e format_on_edit batch operations", () => {
  let preparedBinary: PreparedBinary = initialBinary;
  const harnesses: E2EHarness[] = [];
  const pools: BridgePool[] = [];

  beforeAll(async () => {
    preparedBinary = await prepareBinary();
  });

  afterEach(async () => {
    await Promise.allSettled(pools.splice(0, pools.length).map((pool) => pool.shutdown()));
    await cleanupHarnesses(harnesses);
  });

  test("single-file batch — formatter runs once after all edits", async () => {
    const { h, bridge } = await (async () => {
      const h = await createFormatHarness(preparedBinary, BIOME_TS_PRESET, [
        tsCollapseSpacesShim("biome"),
      ]);
      harnesses.push(h);
      return { h, bridge: h.bridge };
    })();
    const file = h.path("single.ts");
    await writeFile(
      file,
      "export const a = 1;\nexport const b = 2;\nexport const c = 3;\n",
      "utf8",
    );

    const response = await bridge.send("batch", {
      file,
      edits: [
        { match: "export const a = 1;", replacement: "export    const   a   = 10;" },
        { match: "export const b = 2;", replacement: "export    const   b   = 20;" },
        { match: "export const c = 3;", replacement: "export    const   c   = 30;" },
      ],
    });

    expect(response.success).toBe(true);
    expect(response.formatted).toBe(true);
    expect(await readTextFile(file)).toBe(
      "export const a = 10;\nexport const b = 20;\nexport const c = 30;\n",
    );
  });

  test("glob edit across multiple files formats each matched file", async () => {
    const { h, bridge } = await (async () => {
      const h = await createFormatHarness(preparedBinary, BIOME_TS_PRESET, [
        tsCollapseSpacesShim("biome"),
      ]);
      harnesses.push(h);
      return { h, bridge: h.bridge };
    })();
    await mkdir(h.path("glob"), { recursive: true });
    await writeFile(h.path("glob", "a.ts"), "export const OLD_VALUE = 1;\n", "utf8");
    await writeFile(h.path("glob", "b.ts"), "export const OLD_VALUE = 2;\n", "utf8");

    const response = await bridge.send("edit_match", {
      file: h.path("glob", "*.ts"),
      match: "OLD_VALUE",
      replacement: "NEW_VALUE",
    });

    expect(response.success).toBe(true);
    expect(response.total_files).toBe(2);
    expect(
      (response.files as Array<Record<string, unknown>>).every((f) => f.formatted === true),
    ).toBe(true);
    expect(await readTextFile(h.path("glob", "a.ts"))).toBe("export const NEW_VALUE = 1;\n");
    expect(await readTextFile(h.path("glob", "b.ts"))).toBe("export const NEW_VALUE = 2;\n");
  });

  test("glob edit with formatter excluded for some files", async () => {
    const { h, bridge } = await (async () => {
      const h = await createFormatHarness(preparedBinary, BIOME_TS_EXCLUDED_PRESET, [
        biomeExcludedPathShim("biome"),
      ]);
      harnesses.push(h);
      return { h, bridge: h.bridge };
    })();
    await mkdir(h.path("src"), { recursive: true });
    await mkdir(h.path("scratch"), { recursive: true });
    await writeFile(h.path("src", "a.ts"), "export const OLD_VALUE = 1;\n", "utf8");
    await writeFile(h.path("scratch", "b.ts"), "export const OLD_VALUE = 2;\n", "utf8");

    const response = await bridge.send("edit_match", {
      file: h.path("**", "*.ts"),
      match: "OLD_VALUE",
      replacement: "NEW_VALUE",
    });

    expect(response.success).toBe(true);
    expect(response.total_files).toBe(2);
    const files = response.files as Array<Record<string, unknown>>;
    expect(files.find((f) => String(f.file).endsWith("src/a.ts"))?.formatted).toBe(false);
    expect(files.find((f) => String(f.file).endsWith("scratch/b.ts"))?.formatted).toBe(false);
    expect(await readTextFile(h.path("src", "a.ts"))).toBe("export const NEW_VALUE = 1;\n");
    expect(await readTextFile(h.path("scratch", "b.ts"))).toBe("export const NEW_VALUE = 2;\n");
  });

  test("batch with file deletion via empty content range", async () => {
    const { h, bridge } = await (async () => {
      const h = await createFormatHarness(preparedBinary, BIOME_TS_PRESET, [
        tsCollapseSpacesShim("biome"),
      ]);
      harnesses.push(h);
      return { h, bridge: h.bridge };
    })();
    const file = h.path("delete-lines.ts");
    await writeFile(
      file,
      "export const keep = 1;\nexport    const   remove   = 2;\nexport const alsoKeep = 3;\n",
      "utf8",
    );

    const response = await bridge.send("batch", {
      file,
      edits: [{ line_start: 2, line_end: 2, content: "" }],
    });

    expect(response.success).toBe(true);
    expect(response.formatted).toBe(true);
    expect(await readTextFile(file)).toBe("export const keep = 1;\nexport const alsoKeep = 3;\n");
  });
});
