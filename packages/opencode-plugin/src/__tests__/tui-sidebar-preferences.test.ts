import { describe, expect, spyOn, test } from "bun:test";
import * as prefs from "../tui/preferences";
import {
  DEFAULT_PREFS,
  persistCollapsedIfEnabled,
  seedCollapsedFromPrefs,
} from "../tui/preferences";

describe("seedCollapsedFromPrefs", () => {
  test("uses collapsed when boolean", () => {
    expect(
      seedCollapsedFromPrefs({
        ...DEFAULT_PREFS,
        collapsed: true,
        startCollapsed: false,
      }),
    ).toBe(true);
    expect(
      seedCollapsedFromPrefs({
        ...DEFAULT_PREFS,
        collapsed: false,
        startCollapsed: true,
      }),
    ).toBe(false);
  });

  test("falls back to startCollapsed when collapsed is null", () => {
    expect(
      seedCollapsedFromPrefs({
        ...DEFAULT_PREFS,
        collapsed: null,
        startCollapsed: true,
      }),
    ).toBe(true);
    expect(
      seedCollapsedFromPrefs({
        ...DEFAULT_PREFS,
        collapsed: null,
        startCollapsed: false,
      }),
    ).toBe(false);
  });
});

describe("persistCollapsedIfEnabled", () => {
  test("queues write when rememberCollapsed is true", () => {
    const spy = spyOn(prefs, "queueTuiPreferenceUpdate").mockImplementation(() =>
      Promise.resolve(),
    );
    try {
      persistCollapsedIfEnabled({ ...DEFAULT_PREFS, rememberCollapsed: true }, true);
      expect(spy).toHaveBeenCalledWith("aft", ["collapsed"], true);
    } finally {
      spy.mockRestore();
    }
  });

  test("does not queue when rememberCollapsed is false", () => {
    const spy = spyOn(prefs, "queueTuiPreferenceUpdate").mockImplementation(() =>
      Promise.resolve(),
    );
    try {
      persistCollapsedIfEnabled({ ...DEFAULT_PREFS, rememberCollapsed: false }, true);
      expect(spy).not.toHaveBeenCalled();
    } finally {
      spy.mockRestore();
    }
  });
});
