/// <reference path="../bun-test.d.ts" />
import { describe, expect, test } from "bun:test";
import { __trimWaitScanBufferForTests, parseWaitPattern } from "../tools/bash_watch.js";

describe("bash_watch audit regressions", () => {
  test("substring watches retain only the cross-chunk overlap tail", () => {
    const pattern = parseWaitPattern("needle");
    expect(pattern).toBeDefined();

    const trimmed = __trimWaitScanBufferForTests("abcdef", 10, pattern!);

    expect(trimmed.text).toBe("bcdef");
    expect(trimmed.baseOffset).toBe(11);
  });

  test("regex wait patterns keep raw source without compiling JS RegExp", () => {
    const pattern = parseWaitPattern({ regex: "(" });

    expect(pattern).toEqual({ kind: "regex", source: "(" });
    expect("value" in pattern!).toBe(false);
  });

  test("regex watches retain at most a 64 KB rolling scan window", () => {
    const pattern = parseWaitPattern({ regex: "not-found" });
    expect(pattern).toBeDefined();
    const text = "x".repeat(80 * 1024);

    const trimmed = __trimWaitScanBufferForTests(text, 5, pattern!);

    expect(Buffer.byteLength(trimmed.text, "utf8")).toBeLessThanOrEqual(64 * 1024);
    expect(trimmed.baseOffset).toBe(5 + 16 * 1024);
  });
});
