/// <reference path="../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";
import { chmodSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import { withEnv } from "../../../aft-bridge/src/__tests__/test-utils/env-guard.js";
import { normalizeBinaryVersion, probeAftBinary, probeBinaryVersion } from "./binary-probe.js";
import { getAftBinaryName } from "./paths.js";

function writeFakeAft(path: string, body: string): void {
  writeFileSync(path, `#!/bin/sh\n${body}\n`);
  chmodSync(path, 0o755);
}

describe("binary probe version validation", () => {
  test("accepts only semver-shaped aft version output", () => {
    expect(normalizeBinaryVersion("aft 1.2.3\n")).toBe("1.2.3");
    expect(normalizeBinaryVersion("1.2.3-beta.1\n")).toBe("1.2.3-beta.1");
    expect(normalizeBinaryVersion("not-aft 1.2.3\n")).toBeNull();
    expect(normalizeBinaryVersion("hello from another binary\n")).toBeNull();
  });

  test("reports a version-mismatched cache candidate as unmatched", async () => {
    const root = mkdtempSync(join(tmpdir(), "aft-cli-binary-probe-audit-"));
    const cacheDir = join(root, "cache", "bin", "v9.8.7");
    mkdirSync(cacheDir, { recursive: true });
    // The cache candidate path is constructed by us (never the CLI shim), so it
    // is probed without the native-executable guard; a fake shell binary is a
    // valid stand-in here.
    writeFakeAft(join(cacheDir, getAftBinaryName()), 'printf "aft 8.0.0\\n"');

    await withEnv(
      {
        AFT_CACHE_DIR: join(root, "cache"),
        // No native PATH binary available, so resolution must not match.
        PATH: process.env.PATH ?? "",
      },
      () => {
        const probe = probeAftBinary("9.8.7");
        expect(probe.version).toBeNull();
        expect(probe.candidates).toContainEqual(
          expect.objectContaining({ status: "unmatched", version: "8.0.0" }),
        );
      },
    );
  });

  test("reports a non-semver cache candidate as invalid instead of healthy", async () => {
    const root = mkdtempSync(join(tmpdir(), "aft-cli-binary-probe-invalid-"));
    const cacheDir = join(root, "cache", "bin", "v7.7.7");
    mkdirSync(cacheDir, { recursive: true });
    writeFakeAft(join(cacheDir, getAftBinaryName()), 'printf "definitely not aft\\n"');

    await withEnv(
      {
        AFT_CACHE_DIR: join(root, "cache"),
        PATH: process.env.PATH ?? "",
      },
      () => {
        expect(probeBinaryVersion("7.7.7")).toBeNull();
        const probe = probeAftBinary("7.7.7");
        expect(probe.candidates).toContainEqual(expect.objectContaining({ status: "invalid" }));
      },
    );
  });

  test("skips a script-shim `aft` on PATH (fork-bomb guard)", async () => {
    // A `which aft` hit that is a node/sh script shim (e.g. the CLI's own npx
    // bin) must never be probed — probing it re-enters the CLI and fork-bombs.
    // Even though this shim prints a perfectly valid version, it must be
    // filtered out before any --version invocation.
    const root = mkdtempSync(join(tmpdir(), "aft-cli-binary-probe-shim-"));
    const pathDir = join(root, "path");
    mkdirSync(pathDir, { recursive: true });
    writeFakeAft(join(pathDir, getAftBinaryName()), 'printf "aft 7.7.7\\n"');

    await withEnv(
      {
        AFT_CACHE_DIR: join(root, "cache"),
        PATH: `${pathDir}${delimiter}${process.env.PATH ?? ""}`,
      },
      () => {
        const probe = probeAftBinary("7.7.7");
        // The shim is native-filtered, so it never becomes a candidate at all —
        // no "matched" 7.7.7 from it, and resolution finds nothing.
        expect(probe.version).toBeNull();
        expect(
          probe.candidates.some((c) => c.path.startsWith(pathDir) && c.status === "matched"),
        ).toBe(false);
      },
    );
  });
});
