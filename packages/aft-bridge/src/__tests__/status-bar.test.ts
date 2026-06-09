import { describe, expect, test } from "bun:test";
import {
  createStatusBarEmitState,
  formatStatusBar,
  parseStatusBarCounts,
  STATUS_BAR_HEARTBEAT_CALLS,
  type StatusBarCounts,
  shouldEmitStatusBar,
} from "../status-bar.js";

function counts(overrides: Partial<StatusBarCounts> = {}): StatusBarCounts {
  return {
    errors: 0,
    warnings: 0,
    dead_code: 0,
    unused_exports: 0,
    duplicates: 0,
    todos: 0,
    tier2_stale: false,
    ...overrides,
  };
}

describe("parseStatusBarCounts", () => {
  test("parses a well-formed payload", () => {
    const parsed = parseStatusBarCounts({
      errors: 2,
      warnings: 5,
      dead_code: 331,
      unused_exports: 221,
      duplicates: 1159,
      todos: 8,
      tier2_stale: true,
    });
    expect(parsed).toEqual(
      counts({
        errors: 2,
        warnings: 5,
        dead_code: 331,
        unused_exports: 221,
        duplicates: 1159,
        todos: 8,
        tier2_stale: true,
      }),
    );
  });

  test("returns undefined for non-objects", () => {
    expect(parseStatusBarCounts(undefined)).toBeUndefined();
    expect(parseStatusBarCounts(null)).toBeUndefined();
    expect(parseStatusBarCounts("nope")).toBeUndefined();
  });

  test("coerces missing/invalid numbers to 0 and stale to false", () => {
    expect(parseStatusBarCounts({ errors: "x", dead_code: 4 })).toEqual(counts({ dead_code: 4 }));
  });
});

describe("formatStatusBar", () => {
  test("renders the compact ASCII bar", () => {
    expect(
      formatStatusBar(
        counts({
          errors: 2,
          warnings: 5,
          dead_code: 331,
          unused_exports: 221,
          duplicates: 1159,
          todos: 8,
        }),
      ),
    ).toBe("[AFT E2 W5 | D331 U221 C1159 | T8]");
  });

  test("marks stale Tier-2 counts with ~", () => {
    expect(formatStatusBar(counts({ dead_code: 10, tier2_stale: true }))).toBe(
      "[AFT E0 W0 | ~D10 U0 C0 | T0]",
    );
  });
});

describe("shouldEmitStatusBar", () => {
  test("emits on first observation", () => {
    const state = createStatusBarEmitState();
    expect(shouldEmitStatusBar(state, counts({ errors: 1 }))).toBe(true);
  });

  test("suppresses identical consecutive counts, emits on change", () => {
    const state = createStatusBarEmitState();
    shouldEmitStatusBar(state, counts({ errors: 1 }));
    expect(shouldEmitStatusBar(state, counts({ errors: 1 }))).toBe(false);
    // E1 -> E2 is a change.
    expect(shouldEmitStatusBar(state, counts({ errors: 2 }))).toBe(true);
  });

  test("stale flag flip counts as a change", () => {
    const state = createStatusBarEmitState();
    shouldEmitStatusBar(state, counts({ dead_code: 5 }));
    expect(shouldEmitStatusBar(state, counts({ dead_code: 5, tier2_stale: true }))).toBe(true);
  });

  test("re-emits after the heartbeat interval of unchanged calls", () => {
    const state = createStatusBarEmitState();
    const c = counts({ errors: 1 });
    expect(shouldEmitStatusBar(state, c)).toBe(true); // first
    for (let i = 0; i < STATUS_BAR_HEARTBEAT_CALLS - 1; i++) {
      expect(shouldEmitStatusBar(state, c)).toBe(false);
    }
    // callsSinceEmit has now reached the heartbeat threshold.
    expect(shouldEmitStatusBar(state, c)).toBe(true);
  });
});
