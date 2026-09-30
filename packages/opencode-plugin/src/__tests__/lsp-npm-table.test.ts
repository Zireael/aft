import { describe, expect, test } from "bun:test";
import {
  findNpmServerByBinary,
  findNpmServerById,
  isVersionSupported,
  majorVersionOf,
  NPM_LSP_TABLE,
} from "../lsp-npm-table";

describe("npm LSP table", () => {
  test("includes the agreed v0.17.0 packages", () => {
    const ids = NPM_LSP_TABLE.map((s) => s.id);
    expect(ids).toContain("typescript");
    expect(ids).toContain("python");
    expect(ids).toContain("yaml");
    expect(ids).toContain("bash");
    expect(ids).toContain("dockerfile");
    expect(ids).toContain("vue");
    expect(ids).toContain("astro");
    expect(ids).toContain("svelte");
    expect(ids).toContain("biome");
    expect(ids).toContain("php-intelephense");
  });

  test("does NOT include eslint (Pattern E, custom build) or prisma (project-only)", () => {
    const ids = NPM_LSP_TABLE.map((s) => s.id);
    expect(ids).not.toContain("eslint");
    expect(ids).not.toContain("prisma");
  });

  test("ids are unique across the table", () => {
    const ids = NPM_LSP_TABLE.map((s) => s.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  test("npm package names are unique across the table", () => {
    const npmNames = NPM_LSP_TABLE.map((s) => s.npm);
    expect(new Set(npmNames).size).toBe(npmNames.length);
  });

  test("every entry has at least one extension and a non-empty binary name", () => {
    for (const entry of NPM_LSP_TABLE) {
      expect(entry.extensions.length).toBeGreaterThan(0);
      expect(entry.binary.length).toBeGreaterThan(0);
      expect(entry.npm.length).toBeGreaterThan(0);
    }
  });

  test("findNpmServerById finds a known entry", () => {
    expect(findNpmServerById("typescript")?.npm).toBe("typescript-language-server");
  });

  test("findNpmServerById returns undefined for missing id", () => {
    expect(findNpmServerById("nonexistent-id-zzz")).toBeUndefined();
  });

  test("findNpmServerByBinary finds a known entry by binary name", () => {
    const found = findNpmServerByBinary("docker-langserver");
    expect(found?.npm).toBe("dockerfile-language-server-nodejs");
  });
  // typescript-language-server needs lib/tsserver.js from this SDK, and
  // TypeScript 7 (the native compiler) ships none.
  test("typescript-sdk is capped to the 5.x line", () => {
    const sdk = findNpmServerById("typescript-sdk");
    expect(sdk?.npm).toBe("typescript");
    expect(sdk?.supportedMajor).toBe(5);
    if (!sdk) throw new Error("typescript-sdk entry missing");
    expect(isVersionSupported(sdk, "5.9.3")).toBe(true);
    expect(isVersionSupported(sdk, "6.0.2")).toBe(false);
    expect(isVersionSupported(sdk, "7.0.2")).toBe(false);
  });

  test("entries without a cap accept any major", () => {
    const pyright = findNpmServerById("python");
    if (!pyright) throw new Error("python entry missing");
    expect(isVersionSupported(pyright, "99.0.0")).toBe(true);
  });

  test("majorVersionOf reads the leading integer", () => {
    expect(majorVersionOf("5.9.3")).toBe(5);
    expect(majorVersionOf("7.1.0-dev.20260929.1")).toBe(7);
    expect(majorVersionOf("v6.0.0")).toBe(6);
    expect(majorVersionOf("latest")).toBeNull();
  });
});
