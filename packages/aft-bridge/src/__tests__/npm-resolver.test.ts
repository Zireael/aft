import { afterEach, beforeEach, describe, expect, it } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import { npmSpawnEnv, resolveNpm } from "../npm-resolver.js";

/**
 * The resolver is dependency-injected (platform/env/home/execPath) so we can
 * build fake filesystem layouts and assert resolution order without touching
 * the real machine. These tests lock the behavior that fixes the GUI-launch
 * "npm not on PATH" auto-update failure.
 */
describe("resolveNpm", () => {
  let root: string;

  beforeEach(() => {
    root = mkdtempSync(join(tmpdir(), "npm-resolver-test-"));
  });
  afterEach(() => {
    rmSync(root, { recursive: true, force: true });
  });

  function makeNpm(dir: string, name = "npm"): string {
    mkdirSync(dir, { recursive: true });
    const p = join(dir, name);
    writeFileSync(p, "#!/usr/bin/env node\n");
    return p;
  }

  it("resolves npm from PATH first", () => {
    const pathDir = join(root, "path-bin");
    makeNpm(pathDir);
    const result = resolveNpm({
      platform: "linux",
      env: { PATH: `/nonexistent${delimiter}${pathDir}` },
      home: root,
      execPath: "/usr/bin/node",
    });
    expect(result).not.toBeNull();
    expect(result?.binDir).toBe(pathDir);
    expect(result?.command).toBe(join(pathDir, "npm"));
  });

  it("falls back to node-adjacent npm when PATH has none", () => {
    const nodeBin = join(root, "node-install", "bin");
    makeNpm(nodeBin);
    const result = resolveNpm({
      platform: "linux",
      env: { PATH: "/nonexistent" },
      home: root,
      execPath: join(nodeBin, "node"),
    });
    expect(result?.binDir).toBe(nodeBin);
  });

  it("falls back to nvm highest-version when PATH and node-adjacent both miss", () => {
    const nvm = join(root, ".nvm", "versions", "node");
    makeNpm(join(nvm, "v18.0.0", "bin"));
    const v20 = join(nvm, "v20.5.1", "bin");
    makeNpm(v20);
    const result = resolveNpm({
      platform: "linux",
      env: { PATH: "/nonexistent" },
      home: root,
      execPath: "/standalone/bun", // no npm sibling
    });
    // Should pick the highest version (v20.5.1), not v18.
    expect(result?.binDir).toBe(v20);
  });

  it("honors NVM_BIN when set", () => {
    const nvmBin = join(root, "active-nvm-bin");
    makeNpm(nvmBin);
    const result = resolveNpm({
      platform: "linux",
      env: { PATH: "/nonexistent", NVM_BIN: nvmBin },
      home: root,
      execPath: "/standalone/bun",
    });
    expect(result?.binDir).toBe(nvmBin);
  });

  it("falls back to an injected system dir when all else misses", () => {
    const sysDir = join(root, "sys-bin");
    makeNpm(sysDir);
    const result = resolveNpm({
      platform: "linux",
      env: { PATH: "/nonexistent" },
      home: root,
      execPath: "/standalone/bun",
      systemNpmDirs: ["/nonexistent/system", sysDir],
    });
    expect(result?.binDir).toBe(sysDir);
  });

  it("resolves npm.cmd on win32", () => {
    const pathDir = join(root, "win-bin");
    makeNpm(pathDir, "npm.cmd");
    const result = resolveNpm({
      platform: "win32",
      env: { PATH: pathDir },
      home: root,
      execPath: "C:\\node\\node.exe",
    });
    expect(result?.command).toBe(join(pathDir, "npm.cmd"));
  });

  it("returns null when npm is nowhere to be found", () => {
    const result = resolveNpm({
      platform: "linux",
      env: { PATH: "/nonexistent" },
      home: root, // empty tmp dir, no .nvm/.volta/etc
      execPath: "/standalone/bun",
      systemNpmDirs: [], // hermetic: don't pick up a real /usr/local/bin/npm on CI
    });
    expect(result).toBeNull();
  });

  it("ignores relative PATH entries (security: no '.' resolution)", () => {
    // A '.' or relative entry must never be honored.
    const result = resolveNpm({
      platform: "linux",
      env: { PATH: `.${delimiter}relative/bin` },
      home: root,
      execPath: "/standalone/bun",
      systemNpmDirs: [], // hermetic: ignore real system npm on CI
    });
    expect(result).toBeNull();
  });
});

describe("npmSpawnEnv", () => {
  it("prepends binDir to PATH so npm finds its sibling node", () => {
    const env = npmSpawnEnv(
      { command: "/opt/homebrew/bin/npm", binDir: "/opt/homebrew/bin" },
      { PATH: "/usr/bin" },
    );
    expect(env.PATH).toBe(`/opt/homebrew/bin${delimiter}/usr/bin`);
  });

  it("sets PATH to binDir alone when base PATH is empty", () => {
    const env = npmSpawnEnv({ command: "/opt/homebrew/bin/npm", binDir: "/opt/homebrew/bin" }, {});
    expect(env.PATH).toBe("/opt/homebrew/bin");
  });

  it("leaves env unchanged when binDir is null (PATH-resolved)", () => {
    const env = npmSpawnEnv({ command: "npm", binDir: null }, { PATH: "/usr/bin" });
    expect(env.PATH).toBe("/usr/bin");
  });
});
