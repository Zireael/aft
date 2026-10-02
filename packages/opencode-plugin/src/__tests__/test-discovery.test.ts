/// <reference path="../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { join, relative } from "node:path";

// CI selects src/__tests__ and test/, so tests elsewhere under src/ are not
// run there and can silently become stale. Keep source tests in src/__tests__.
function testFilesOutsideTestsDir(root: string): string[] {
  const found: string[] = [];
  const walk = (dir: string) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) {
        if (path !== join(root, "__tests__") && entry.name !== "node_modules") walk(path);
      } else if (/\.test\.tsx?$/.test(entry.name)) {
        found.push(relative(root, path));
      }
    }
  };
  walk(root);
  return found;
}

describe("test discovery", () => {
  test("every test file lives under src/__tests__, where CI looks", () => {
    expect(testFilesOutsideTestsDir(join(import.meta.dir, ".."))).toEqual([]);
  });
});

const packageRoot = join(import.meta.dir, "../..");
const separatelyRunProbes = ["test/load-matrix/load-matrix.ts"];

function uncoveredPackageTests(): string[] {
  const manifest = JSON.parse(readFileSync(join(packageRoot, "package.json"), "utf8"));
  const unitScript = manifest.scripts["test:unit"] as string;
  // Only accept directories actually selected by the CI unit command. A new
  // directory or a removed script argument must not silently orphan tests.
  const tokens = (unitScript.match(/'[^']*'|"[^"]*"|\S+/g) ?? []).map((token) =>
    token.replace(/^['"]|['"]$/g, ""),
  );
  expect(tokens.splice(0, 2)).toEqual(["bun", "test"]);
  const selected: string[] = [];
  const ignored: string[] = [];
  for (let i = 0; i < tokens.length; i++) {
    const token = tokens[i];
    if (token === "--path-ignore-patterns") ignored.push(tokens[++i]);
    else {
      // Unknown flags can change discovery (for example a name filter).
      expect(token.startsWith("-")).toBe(false);
      selected.push(token.replace(/^\.\//, "").replace(/\/$/, ""));
    }
  }
  const found: string[] = [];
  const walk = (dir: string) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) {
        if (!["node_modules", "dist", "tmp", ".git", "tui-compiled"].includes(entry.name))
          walk(path);
      } else {
        const file = relative(packageRoot, path).replaceAll("\\", "/");
        const conventional = /[._](test|spec)\.[cm]?[jt]sx?$/.test(file);
        const explicitProbe =
          file.startsWith("test/") &&
          /\.[cm]?[jt]sx?$/.test(file) &&
          /from\s*["']bun:test["']/.test(readFileSync(path, "utf8"));
        if (!conventional && !explicitProbe) continue;
        if (separatelyRunProbes.includes(file)) continue;
        const inE2eMatrix = file.startsWith("src/__tests__/e2e/");
        const inUnitSuite =
          conventional &&
          selected.some((root) => file.startsWith(`${root}/`)) &&
          !ignored.some((pattern) => new Bun.Glob(pattern).match(file));
        if (!inUnitSuite && !inE2eMatrix) found.push(file);
      }
    }
  };
  walk(packageRoot);
  return found.sort();
}

describe("CI test discovery", () => {
  test("every package test is selected by a CI command", () => {
    const workflow = readFileSync(
      join(packageRoot, "../../.github/workflows/_unit-suite.yml"),
      "utf8",
    );
    const rootManifest = JSON.parse(readFileSync(join(packageRoot, "../../package.json"), "utf8"));
    expect(workflow).toContain("run: bun run test:unit");
    expect(rootManifest.scripts["test:unit"]).toBe("bun run --filter './packages/*' test:unit");
    // test:unit excludes src/__tests__/e2e; the workflow's plugin-e2e-linux
    // matrix runs that subtree separately with real transports and longer timeouts.
    expect(workflow).toContain("package: opencode-plugin");
    expect(workflow).toContain("bun test --timeout 30000 src/__tests__/e2e/");
    const betaWorkflow = readFileSync(
      join(packageRoot, "../../.github/workflows/beta-pin-gate.yml"),
      "utf8",
    );
    for (const file of separatelyRunProbes) {
      expect(betaWorkflow).toContain(`./packages/opencode-plugin/${file}`);
    }
    expect(uncoveredPackageTests()).toEqual([]);
  });
});
