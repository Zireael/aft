/// <reference path="../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";
import { readdirSync } from "node:fs";
import { join, relative } from "node:path";

// CI runs `bun test src/__tests__`, so a test file anywhere else under src/
// never runs there and silently rots (two read-vision tests once did, for
// months, behind a renamed config key). Keep every test inside src/__tests__.
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
