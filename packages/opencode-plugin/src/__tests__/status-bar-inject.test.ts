import { describe, expect, test } from "bun:test";
import type { StatusBarCounts } from "@cortexkit/aft-bridge";
import { clearStatusBarSession, statusBarSuffixForSession } from "../status-bar-inject.js";

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

describe("statusBarSuffixForSession", () => {
  test("returns empty when counts are absent (no scan yet)", () => {
    expect(statusBarSuffixForSession("s-none", undefined)).toBe("");
  });

  test("emits on first observation, suppresses unchanged, re-emits on change", () => {
    const sid = "s-1";
    clearStatusBarSession(sid);
    const first = statusBarSuffixForSession(sid, counts({ errors: 1, dead_code: 5 }));
    expect(first).toContain("[AFT E1 W0 | D5 U0 C0 | T0]");
    // Unchanged → suppressed.
    expect(statusBarSuffixForSession(sid, counts({ errors: 1, dead_code: 5 }))).toBe("");
    // Error count changed → re-emit.
    expect(statusBarSuffixForSession(sid, counts({ errors: 2, dead_code: 5 }))).toContain(
      "[AFT E2 W0 | D5 U0 C0 | T0]",
    );
  });

  test("per-session state is independent", () => {
    clearStatusBarSession("a");
    clearStatusBarSession("b");
    expect(statusBarSuffixForSession("a", counts({ errors: 1 }))).not.toBe("");
    // Session b has never seen a bar → still emits on its own first observation.
    expect(statusBarSuffixForSession("b", counts({ errors: 1 }))).not.toBe("");
  });

  test("renders the stale marker", () => {
    const sid = "s-stale";
    clearStatusBarSession(sid);
    expect(statusBarSuffixForSession(sid, counts({ dead_code: 10, tier2_stale: true }))).toContain(
      "~D10",
    );
  });
});
