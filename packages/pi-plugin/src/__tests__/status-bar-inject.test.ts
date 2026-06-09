import { describe, expect, test } from "bun:test";
import type { StatusBarCounts } from "@cortexkit/aft-bridge";
import { clearStatusBarSession, statusBarBlockForSession } from "../status-bar-inject.js";

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

describe("statusBarBlockForSession (Pi)", () => {
  test("returns undefined when counts are absent (no scan yet)", () => {
    expect(statusBarBlockForSession("s-none", undefined)).toBeUndefined();
  });

  test("emits a bare bar block (no leading newlines) on change", () => {
    const sid = "pi-1";
    clearStatusBarSession(sid);
    const first = statusBarBlockForSession(sid, counts({ errors: 1, dead_code: 5 }));
    expect(first).toBe("[AFT E1 W0 | D5 U0 C0 | T0]");
    // Unchanged → suppressed.
    expect(statusBarBlockForSession(sid, counts({ errors: 1, dead_code: 5 }))).toBeUndefined();
    // Changed → re-emit.
    expect(statusBarBlockForSession(sid, counts({ errors: 0, dead_code: 5 }))).toBe(
      "[AFT E0 W0 | D5 U0 C0 | T0]",
    );
  });

  test("renders the stale marker", () => {
    const sid = "pi-stale";
    clearStatusBarSession(sid);
    expect(statusBarBlockForSession(sid, counts({ duplicates: 9, tier2_stale: true }))).toBe(
      "[AFT E0 W0 | ~D0 U0 C9 | T0]",
    );
  });
});
