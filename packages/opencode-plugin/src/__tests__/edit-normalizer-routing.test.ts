/// <reference path="../bun-test.d.ts" />

import { expect, test } from "bun:test";
import { readdir, readFile } from "node:fs/promises";
import { relative, resolve } from "node:path";
import ts from "typescript";
import { prepareOpenCodeArguments } from "../normalize-schemas.js";

const REPO_ROOT = resolve(import.meta.dir, "../../../..");
const PRODUCTION_SOURCE_ROOTS = [
  "packages/aft-bridge/src",
  "packages/opencode-plugin/src",
  "packages/pi-plugin/src",
] as const;
const GUARDED_CALLS = new Set(["prepareCanonicalEditArguments", "prepareOpenCodeArguments"]);

async function productionTypescriptFiles(root: string): Promise<string[]> {
  const files: string[] = [];

  async function walk(directory: string): Promise<void> {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      if (entry.isDirectory()) {
        if (entry.name === "__tests__" || entry.name === "dist" || entry.name === "tui-compiled") {
          continue;
        }
        await walk(resolve(directory, entry.name));
      } else if (entry.isFile() && entry.name.endsWith(".ts") && !entry.name.endsWith(".test.ts")) {
        files.push(resolve(directory, entry.name));
      }
    }
  }

  await walk(root);
  return files;
}

async function guardedCallSites(): Promise<string[]> {
  const sites: string[] = [];
  const guardedDefinitions = new Set<string>();
  for (const sourceRoot of PRODUCTION_SOURCE_ROOTS) {
    for (const file of await productionTypescriptFiles(resolve(REPO_ROOT, sourceRoot))) {
      const source = ts.createSourceFile(
        file,
        await readFile(file, "utf8"),
        ts.ScriptTarget.Latest,
        true,
        ts.ScriptKind.TS,
      );
      const visit = (node: ts.Node): void => {
        if (ts.isFunctionDeclaration(node) && node.name && GUARDED_CALLS.has(node.name.text)) {
          guardedDefinitions.add(node.name.text);
        }
        if (
          ts.isCallExpression(node) &&
          ts.isIdentifier(node.expression) &&
          GUARDED_CALLS.has(node.expression.text)
        ) {
          sites.push(
            `${relative(REPO_ROOT, file)}:${node.expression.text}:${node.arguments.length}`,
          );
        }
        ts.forEachChild(node, visit);
      };
      visit(source);
    }
  }
  expect(guardedDefinitions).toEqual(GUARDED_CALLS);
  return sites.sort();
}

test("edit argument normalizer call sites stay behind the audited plugin boundaries", async () => {
  expect(await guardedCallSites()).toEqual([
    "packages/opencode-plugin/src/index.ts:prepareOpenCodeArguments:3",
    "packages/opencode-plugin/src/normalize-schemas.ts:prepareCanonicalEditArguments:2",
    "packages/opencode-plugin/src/normalize-schemas.ts:prepareOpenCodeArguments:3",
    "packages/opencode-plugin/src/v2-tool-hooks.ts:prepareOpenCodeArguments:3",
    "packages/pi-plugin/src/tools/_shared.ts:prepareCanonicalEditArguments:2",
  ]);
});

test("hashline edit arguments bypass canonical preparation at the shared decision site", () => {
  const raw = { patch: "[file.ts#TAG]\nPUT 1:\n+replacement" };

  expect(prepareOpenCodeArguments("edit", raw, { hashlineEffective: true })).toBe(raw);
  expect(() => prepareOpenCodeArguments("edit", raw)).toThrow('Unrecognized keys: "patch"');
});
