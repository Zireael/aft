/// <reference path="../bun-test.d.ts" />

// The edit-result diff trims lines shared at the start and end of both files
// before building its LCS table. The rendered diff must stay exactly what the
// untrimmed table produced, including how ties between equally long common
// subsequences are broken (which line of a repeated run is shown as removed).
// The oracle below is the untrimmed algorithm, kept verbatim.

import { describe, expect, test } from "bun:test";
import { _diffLinesForTest, _diffTableCellsForTest } from "../tools/hoisted.js";

type DiffOp =
  | { tag: "eq"; beforeIdx: number; afterIdx: number; line: string }
  | { tag: "del"; beforeIdx: number; line: string }
  | { tag: "ins"; afterIdx: number; line: string };

function untrimmedDiffLines(a: readonly string[], b: readonly string[]): DiffOp[] {
  const n = a.length;
  const m = b.length;
  const dp = new Uint32Array((n + 1) * (m + 1));
  const w = m + 1;
  for (let i = 1; i <= n; i++) {
    for (let j = 1; j <= m; j++) {
      if (a[i - 1] === b[j - 1]) {
        dp[i * w + j] = dp[(i - 1) * w + (j - 1)] + 1;
      } else {
        const up = dp[(i - 1) * w + j];
        const left = dp[i * w + (j - 1)];
        dp[i * w + j] = up >= left ? up : left;
      }
    }
  }
  const ops: DiffOp[] = [];
  let i = n;
  let j = m;
  while (i > 0 && j > 0) {
    if (a[i - 1] === b[j - 1]) {
      ops.push({ tag: "eq", beforeIdx: i - 1, afterIdx: j - 1, line: a[i - 1] });
      i--;
      j--;
    } else if (dp[(i - 1) * w + j] >= dp[i * w + (j - 1)]) {
      ops.push({ tag: "del", beforeIdx: i - 1, line: a[i - 1] });
      i--;
    } else {
      ops.push({ tag: "ins", afterIdx: j - 1, line: b[j - 1] });
      j--;
    }
  }
  while (i > 0) {
    ops.push({ tag: "del", beforeIdx: i - 1, line: a[i - 1] });
    i--;
  }
  while (j > 0) {
    ops.push({ tag: "ins", afterIdx: j - 1, line: b[j - 1] });
    j--;
  }
  ops.reverse();
  return ops;
}

/** Deterministic PRNG so a failure reproduces. */
function rng(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state * 1664525 + 1013904223) >>> 0;
    return state / 0x100000000;
  };
}

describe("diffLines prefix/suffix trimming", () => {
  test("matches the untrimmed diff on random edits with heavy line repetition", () => {
    // A tiny alphabet with blank lines and braces makes ties common, which is
    // where a trimmed table could pick a different line of a repeated run.
    const alphabet = ["", "}", "a", "b", "  return x;"];
    const random = rng(0x5eed);
    const pick = () => alphabet[Math.floor(random() * alphabet.length)];
    for (let round = 0; round < 3000; round++) {
      const before = Array.from({ length: Math.floor(random() * 25) }, pick);
      const after = [...before];
      const edits = 1 + Math.floor(random() * 4);
      for (let e = 0; e < edits; e++) {
        const at = Math.floor(random() * (after.length + 1));
        const kind = random();
        if (kind < 0.4) after.splice(at, 0, pick());
        else if (kind < 0.8) after.splice(at, 1);
        else after.splice(at, 1, pick());
      }
      expect(_diffLinesForTest(before, after)).toEqual(untrimmedDiffLines(before, after));
    }
  });

  test("matches the untrimmed diff for insertions next to an identical line", () => {
    const cases: Array<[string[], string[]]> = [
      [["x", "x"], ["x"]],
      [["x"], ["x", "x"]],
      [
        ["a", "", "b"],
        ["a", "", "new", "", "b"],
      ],
      [
        ["{", "}", "}"],
        ["{", "}", "}", "}"],
      ],
      [[], ["a"]],
      [["a"], []],
      [
        ["same", "same"],
        ["same", "same"],
      ],
    ];
    for (const [before, after] of cases) {
      expect(_diffLinesForTest(before, after)).toEqual(untrimmedDiffLines(before, after));
    }
  });

  // Lock-in: a one-line edit in a large file must not build a file-sized table.
  test("a one-line edit in a 3000-line file builds a 2x2 table, not 3001x3001", () => {
    const before = Array.from({ length: 3000 }, (_, i) => `// line ${i + 1}`);
    const after = before.map((line, i) => (i === 1500 ? "// EDITED" : line));
    const cellsBefore = _diffTableCellsForTest();
    const ops = _diffLinesForTest(before, after);
    expect(_diffTableCellsForTest() - cellsBefore).toBe(4);
    expect(ops).toEqual(untrimmedDiffLines(before, after));
  });
});
