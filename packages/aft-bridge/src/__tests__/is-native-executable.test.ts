/// <reference path="../bun-test.d.ts" />

import { afterAll, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { isNativeExecutable } from "../resolver.js";

// Regression coverage for the binary-probe fork bomb: `which aft` could resolve
// to the @cortexkit/aft CLI's own node-script shim (npx prepends
// node_modules/.bin to PATH; the CLI bin is named `aft`). Probing it with
// --version re-enters the CLI and spawns opencode/pi --version recursively.
// isNativeExecutable rejects shebang shims so the resolver never probes itself.
const root = mkdtempSync(join(tmpdir(), "aft-native-exe-"));

function fixture(name: string, bytes: Buffer | string): string {
  const p = join(root, name);
  writeFileSync(p, bytes);
  return p;
}

afterAll(() => {
  rmSync(root, { recursive: true, force: true });
});

describe("isNativeExecutable", () => {
  test("rejects a node-script shim (#! shebang) — the fork-bomb vector", () => {
    const shim = fixture("aft-shim", "#!/usr/bin/env node\nconsole.log('cli');\n");
    expect(isNativeExecutable(shim)).toBe(false);
  });

  test("rejects an sh shebang shim", () => {
    expect(isNativeExecutable(fixture("aft-sh", "#!/bin/sh\nexec node x\n"))).toBe(false);
  });

  test("accepts a Mach-O LE 64-bit binary (cffaedfe)", () => {
    expect(
      isNativeExecutable(fixture("macho64", Buffer.from([0xcf, 0xfa, 0xed, 0xfe, 0x07]))),
    ).toBe(true);
  });

  test("accepts a Mach-O fat binary (cafebabe)", () => {
    expect(isNativeExecutable(fixture("machofat", Buffer.from([0xca, 0xfe, 0xba, 0xbe])))).toBe(
      true,
    );
  });

  test("accepts an ELF binary (7f 45 4c 46)", () => {
    expect(isNativeExecutable(fixture("elf", Buffer.from([0x7f, 0x45, 0x4c, 0x46, 0x02])))).toBe(
      true,
    );
  });

  test("accepts a Windows PE binary (MZ)", () => {
    expect(isNativeExecutable(fixture("pe.exe", Buffer.from([0x4d, 0x5a, 0x90, 0x00])))).toBe(true);
  });

  test("rejects an arbitrary text file", () => {
    expect(isNativeExecutable(fixture("notes.txt", "just some text\n"))).toBe(false);
  });

  test("rejects a non-existent path", () => {
    expect(isNativeExecutable(join(root, "does-not-exist"))).toBe(false);
  });

  test("rejects a too-short file", () => {
    expect(isNativeExecutable(fixture("onebyte", Buffer.from([0x23])))).toBe(false);
  });
});
