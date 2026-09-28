import { describe, expect, test } from "bun:test";
import {
  coerceAliasedStringParam,
  coerceBoolean,
  coerceJsonCollectionParam,
  coerceStringArray,
  coerceTargetParam,
} from "../coerce.js";

describe("coerceJsonCollectionParam", () => {
  test("parses JSON targets and sections without altering plain sections", () => {
    expect(coerceJsonCollectionParam('[{"path":"a.ts","symbol":"foo"}]', "targets")).toEqual([
      { path: "a.ts", symbol: "foo" },
    ]);
    expect(coerceJsonCollectionParam('["dead_code"]', "sections")).toEqual(["dead_code"]);
    expect(coerceJsonCollectionParam("all", "sections")).toBe("all");
  });

  test("reports malformed JSON with the parameter name", () => {
    expect(() => coerceJsonCollectionParam("[{bad", "targets")).toThrow(/targets.*valid JSON/);
    expect(() => coerceJsonCollectionParam("[bad", "sections")).toThrow(/sections.*valid JSON/);
  });
});

describe("coerceTargetParam", () => {
  test("passes a single path/URL string through unchanged", () => {
    expect(coerceTargetParam("src/app.ts")).toBe("src/app.ts");
    expect(coerceTargetParam("https://x/y")).toBe("https://x/y");
    expect(coerceTargetParam("my file.ts")).toBe("my file.ts");
  });

  test("parses a JSON-stringified array (the aft_outline bug)", () => {
    expect(coerceTargetParam('["src/a", "src/b"]')).toEqual(["src/a", "src/b"]);
    expect(coerceTargetParam('  ["only"]  ')).toEqual(["only"]);
  });

  test("passes a real array through, dropping empties/non-strings", () => {
    expect(coerceTargetParam(["a", "b"])).toEqual(["a", "b"]);
    expect(coerceTargetParam(["a", "", 3, null, "b"] as unknown)).toEqual(["a", "b"]);
  });

  test("treats a malformed bracketed string as a single literal path", () => {
    expect(coerceTargetParam("[not json")).toBe("[not json");
    // A path that merely contains brackets but isn't a JSON array stays a string.
    expect(coerceTargetParam("weird[name].ts")).toBe("weird[name].ts");
  });
});

describe("coerceAliasedStringParam", () => {
  test("uses the declared field when it is already present", () => {
    expect(coerceAliasedStringParam("path.ts", "alias.ts")).toBe("path.ts");
    expect(coerceAliasedStringParam("", "alias.ts")).toBe("");
  });

  test("falls back to a non-empty alias only when the declared field is absent", () => {
    expect(coerceAliasedStringParam(undefined, "alias.ts")).toBe("alias.ts");
    expect(coerceAliasedStringParam(undefined, "  spaced path.ts  ")).toBe("  spaced path.ts  ");
  });

  test("does not let an alias override an explicit malformed field", () => {
    expect(coerceAliasedStringParam(123, "alias.ts")).toBeUndefined();
    expect(coerceAliasedStringParam(null, "alias.ts")).toBeUndefined();
  });

  test("ignores empty and non-string aliases", () => {
    expect(coerceAliasedStringParam(undefined, "")).toBeUndefined();
    expect(coerceAliasedStringParam(undefined, "   ")).toBeUndefined();
    expect(coerceAliasedStringParam(undefined, 7)).toBeUndefined();
  });
});

describe("coerceBoolean", () => {
  test("passes real booleans through", () => {
    expect(coerceBoolean(true)).toBe(true);
    expect(coerceBoolean(false)).toBe(false);
  });

  test("coerces the stringified booleans models emit (the recursive bug)", () => {
    expect(coerceBoolean("true")).toBe(true);
    expect(coerceBoolean("TRUE")).toBe(true);
    expect(coerceBoolean("  true  ")).toBe(true);
    expect(coerceBoolean("1")).toBe(true);
    expect(coerceBoolean(1)).toBe(true);
  });

  test("treats everything else as false (tight truthy set for safety gates)", () => {
    expect(coerceBoolean("false")).toBe(false);
    expect(coerceBoolean("0")).toBe(false);
    expect(coerceBoolean(0)).toBe(false);
    expect(coerceBoolean(2)).toBe(false);
    expect(coerceBoolean("yes")).toBe(false);
    expect(coerceBoolean("")).toBe(false);
    expect(coerceBoolean(undefined)).toBe(false);
    expect(coerceBoolean(null)).toBe(false);
    expect(coerceBoolean({})).toBe(false);
    expect(coerceBoolean([])).toBe(false);
  });

  test("supports default-true booleans with explicit false-like values", () => {
    expect(coerceBoolean(undefined, true)).toBe(true);
    expect(coerceBoolean("false", true)).toBe(false);
    expect(coerceBoolean("0", true)).toBe(false);
    expect(coerceBoolean(0, true)).toBe(false);
    expect(coerceBoolean(false, true)).toBe(false);
    expect(coerceBoolean("true", true)).toBe(true);
    expect(coerceBoolean("anything else", true)).toBe(true);
  });
});

describe("coerceStringArray", () => {
  test("passes a real string array through, dropping empties + non-strings", () => {
    expect(coerceStringArray(["a.ts", "b.ts"])).toEqual(["a.ts", "b.ts"]);
    expect(coerceStringArray(["a.ts", "", "b.ts"])).toEqual(["a.ts", "b.ts"]);
    expect(coerceStringArray(["a.ts", 3, null, "b.ts"] as unknown)).toEqual(["a.ts", "b.ts"]);
  });

  test("parses a JSON-stringified array (the crash trigger)", () => {
    expect(coerceStringArray('["a.ts","b.ts"]')).toEqual(["a.ts", "b.ts"]);
    expect(coerceStringArray('  ["only.ts"]  ')).toEqual(["only.ts"]);
  });

  test("wraps a single bare string as a one-element array", () => {
    expect(coerceStringArray("a.ts")).toEqual(["a.ts"]);
  });

  test("preserves spaces in a single path (no splitting)", () => {
    expect(coerceStringArray("my file.ts")).toEqual(["my file.ts"]);
    expect(coerceStringArray("a/b c/d.ts")).toEqual(["a/b c/d.ts"]);
  });

  test("returns empty for null/undefined/empty/other shapes", () => {
    expect(coerceStringArray(undefined)).toEqual([]);
    expect(coerceStringArray(null)).toEqual([]);
    expect(coerceStringArray("")).toEqual([]);
    expect(coerceStringArray("   ")).toEqual([]);
    expect(coerceStringArray([])).toEqual([]);
    expect(coerceStringArray(42)).toEqual([]);
    expect(coerceStringArray({ files: "a.ts" })).toEqual([]);
  });

  test("falls back to single-string when JSON is malformed", () => {
    // Looks array-ish but isn't valid JSON -> treat as a single path.
    expect(coerceStringArray("[not json")).toEqual(["[not json"]);
  });
});
