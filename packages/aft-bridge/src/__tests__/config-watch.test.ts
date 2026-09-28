/// <reference path="../bun-test.d.ts" />

import { afterEach, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, unlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  aftLiveConfigKeys,
  applyLiveConfigKeys,
  CONFIG_LIVE_KEEP_NOTE,
  type LiveConfigLoad,
  liveConfigReloadLogLine,
  type ResolvedBashForLiveReload,
  startLiveConfigReload,
  watchAftConfigFiles,
} from "../config-watch.js";

type TestConfig = {
  restrict_to_project_root?: boolean;
  disabled_tools?: string[];
  bash?: boolean | Record<string, unknown>;
  inspect?: Record<string, unknown>;
};

/** A small stand-in for a host's `resolveBashConfig`. */
function resolveBash(config: TestConfig): ResolvedBashForLiveReload & Record<string, unknown> {
  const top = config.bash;
  const object = typeof top === "object" && top !== null ? top : {};
  return {
    enabled: top !== false,
    compress: top === true || (object.compress ?? true) === true,
    background: top === true || (object.background ?? true) === true,
    host_fallback: object.host_fallback === true,
    subagent_background: object.subagent_background !== false,
    foreground_wait_window_ms: (object.foreground_wait_window_ms as number) ?? 15_000,
    watch_sync_max_ms: (object.watch_sync_max_ms as number) ?? 120_000,
  };
}

const KEYS = aftLiveConfigKeys<TestConfig>(resolveBash);
const tempDirs: string[] = [];

afterEach(() => {
  for (const dir of tempDirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

function tempDir(): string {
  const dir = mkdtempSync(join(tmpdir(), "aft-config-watch-"));
  tempDirs.push(dir);
  return dir;
}

describe("applyLiveConfigKeys", () => {
  test("applies a live key and defers the rest", () => {
    const current: TestConfig = { restrict_to_project_root: false, disabled_tools: [] };
    const next: TestConfig = { restrict_to_project_root: true, disabled_tools: ["aft_zoom"] };

    const result = applyLiveConfigKeys(current, next, KEYS);

    expect(result.applied).toEqual(["restrict_to_project_root"]);
    expect(result.deferred).toEqual(["disabled_tools"]);
    expect(result.config.restrict_to_project_root).toBe(true);
    expect(result.config.disabled_tools).toEqual([]);
    // The caller's old snapshot is not mutated.
    expect(current.restrict_to_project_root).toBe(false);
  });

  test("a live bash key keeps every other bash setting as loaded", () => {
    const current: TestConfig = { bash: true };
    const next: TestConfig = { bash: { watch_sync_max_ms: 5_000, compress: false } };

    const result = applyLiveConfigKeys(current, next, KEYS);

    expect(result.applied).toEqual(["bash.watch_sync_max_ms"]);
    const bash = resolveBash(result.config);
    expect(bash.watch_sync_max_ms).toBe(5_000);
    expect(bash.compress).toBe(true);
    expect(bash.background).toBe(true);
  });

  test("the log line names applied and deferred keys", () => {
    expect(liveConfigReloadLogLine(["restrict_to_project_root"], ["disabled_tools"])).toBe(
      "config reload applied=[restrict_to_project_root] deferred=[disabled_tools] (deferred keys apply on next connect/restart)",
    );
  });
});

describe("startLiveConfigReload", () => {
  function harness(initial: TestConfig, paths: string[] = []) {
    let config = initial;
    let nextLoad: LiveConfigLoad<TestConfig> = { ok: true, config: initial };
    const errors: string[] = [];
    const logs: string[] = [];
    const reload = startLiveConfigReload<TestConfig>({
      paths,
      load: () => nextLoad,
      keys: KEYS,
      getConfig: () => config,
      setConfig: (next) => {
        config = next;
      },
      log: (line) => logs.push(line),
      reportError: (message) => errors.push(message),
      watch: false,
    });
    return {
      reload,
      errors,
      logs,
      config: () => config,
      loads: (load: LiveConfigLoad<TestConfig>) => {
        nextLoad = load;
      },
    };
  }

  test("an invalid file keeps the last valid config and is reported once", () => {
    const h = harness({ restrict_to_project_root: true });
    h.loads({ ok: false, message: "AFT config at /x failed to parse: bad" });

    h.reload.reload();
    h.reload.reload();

    expect(h.config().restrict_to_project_root).toBe(true);
    expect(h.errors).toEqual([`AFT config at /x failed to parse: bad. ${CONFIG_LIVE_KEEP_NOTE}`]);
  });

  test("a deleted config file keeps the last valid config", () => {
    const dir = tempDir();
    const file = join(dir, "aft.jsonc");
    writeFileSync(file, "{}");
    const h = harness({ restrict_to_project_root: true }, [file]);
    unlinkSync(file);
    // The loader would resolve a missing file as empty; the reload must not
    // get that far.
    h.loads({ ok: true, config: { restrict_to_project_root: false } });

    h.reload.reload();

    expect(h.config().restrict_to_project_root).toBe(true);
    expect(h.errors[0]).toContain("was deleted");
  });

  test("a valid edit swaps in a new config object", () => {
    const initial: TestConfig = { restrict_to_project_root: false };
    const h = harness(initial);
    h.loads({ ok: true, config: { restrict_to_project_root: true } });

    h.reload.reload();

    expect(h.config()).not.toBe(initial);
    expect(h.config().restrict_to_project_root).toBe(true);
    expect(h.logs).toEqual(["config reload applied=[restrict_to_project_root]"]);
  });
});

describe("watchAftConfigFiles", () => {
  test("an edit, and a config directory created later, call onChange", async () => {
    const dir = tempDir();
    const file = join(dir, ".cortexkit", "aft.jsonc");
    let changes = 0;
    const stop = watchAftConfigFiles({
      paths: [file],
      debounceMs: 20,
      onChange: () => {
        changes += 1;
      },
    });
    const waitFor = async (count: number) => {
      const deadline = Date.now() + 5_000;
      while (changes < count && Date.now() < deadline) {
        await new Promise((resolve) => setTimeout(resolve, 25));
      }
      expect(changes).toBeGreaterThanOrEqual(count);
    };
    try {
      mkdirSync(join(dir, ".cortexkit"));
      writeFileSync(file, "{}");
      await waitFor(1);
      await new Promise((resolve) => setTimeout(resolve, 100));
      const seen = changes;
      writeFileSync(file, '{ "restrict_to_project_root": true }');
      await waitFor(seen + 1);
    } finally {
      stop();
    }
  });
});
