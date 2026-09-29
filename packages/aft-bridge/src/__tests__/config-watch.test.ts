/// <reference path="../bun-test.d.ts" />

import { afterEach, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
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
  function harness(initial: TestConfig, initialSources: string[] = []) {
    let config = initial;
    let nextLoad: LiveConfigLoad<TestConfig> = {
      ok: true,
      config: initial,
      sources: initialSources,
    };
    const errors: string[] = [];
    const logs: string[] = [];
    const reload = startLiveConfigReload<TestConfig>({
      paths: [],
      load: () => nextLoad,
      initialSources,
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

  test("a load that no longer read a file it relied on keeps the last valid config", () => {
    // The file vanished before the loader looked, so the loader resolved it as
    // absent without any error; only its record of what it read tells.
    const h = harness({ restrict_to_project_root: true }, ["/cfg/user/aft.jsonc"]);
    h.loads({ ok: true, config: { restrict_to_project_root: false }, sources: [] });

    h.reload.reload();

    expect(h.config().restrict_to_project_root).toBe(true);
    expect(h.errors[0]).toContain("/cfg/user/aft.jsonc was deleted");
  });

  test("a file absent at startup may appear later", () => {
    const h = harness({ restrict_to_project_root: false }, []);
    h.loads({
      ok: true,
      config: { restrict_to_project_root: true },
      sources: ["/cfg/user/aft.jsonc"],
    });
    h.reload.reload();
    expect(h.config().restrict_to_project_root).toBe(true);
  });

  test("a valid edit swaps in a new config object", () => {
    const initial: TestConfig = { restrict_to_project_root: false };
    const h = harness(initial);
    h.loads({ ok: true, config: { restrict_to_project_root: true }, sources: [] });

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

async function waitUntil(
  predicate: () => boolean,
  label: string,
  timeoutMs = 5_000,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (Date.now() > deadline) throw new Error(`timed out: ${label}`);
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
}

describe("watchAftConfigFiles recovery", () => {
  test("a replaced config directory is watched again", async () => {
    const dir = tempDir();
    const configDir = join(dir, ".cortexkit");
    const file = join(configDir, "aft.jsonc");
    mkdirSync(configDir);
    writeFileSync(file, "{}");
    let changes = 0;
    const stop = watchAftConfigFiles({
      paths: [file],
      debounceMs: 20,
      onChange: () => {
        changes += 1;
      },
    });
    try {
      rmSync(configDir, { recursive: true, force: true });
      await waitUntil(() => changes >= 1, "deletion noticed");
      mkdirSync(configDir);
      writeFileSync(file, '{ "restrict_to_project_root": true }');
      await waitUntil(() => changes >= 2, "recreated file noticed");
      await new Promise((resolve) => setTimeout(resolve, 300));
      const seen = changes;
      writeFileSync(file, '{ "restrict_to_project_root": false }');
      await waitUntil(() => changes > seen, "edit in the recreated directory noticed");
    } finally {
      stop();
    }
  });

  test("steady unrelated activity in the directory does not starve a config edit", async () => {
    const dir = tempDir();
    const file = join(dir, "aft.jsonc");
    writeFileSync(file, "{}");
    let changes = 0;
    const stop = watchAftConfigFiles({
      paths: [file],
      debounceMs: 150,
      onChange: () => {
        changes += 1;
      },
    });
    let n = 0;
    const noise = setInterval(() => writeFileSync(join(dir, "status"), String(n++)), 40);
    try {
      writeFileSync(file, '{ "restrict_to_project_root": true }');
      await waitUntil(() => changes >= 1, "config edit applied under noise", 3_000);
    } finally {
      clearInterval(noise);
      stop();
    }
  });
});

describe("startLiveConfigReload start-up", () => {
  test("an edit made before the watch started is applied at once", () => {
    let config: TestConfig = { restrict_to_project_root: false };
    const reload = startLiveConfigReload<TestConfig>({
      paths: [join(tempDir(), "aft.jsonc")],
      // The host loaded its config before this edit; the file now says true.
      load: () => ({ ok: true, config: { restrict_to_project_root: true }, sources: [] }),
      initialSources: [],
      keys: KEYS,
      getConfig: () => config,
      setConfig: (next) => {
        config = next;
      },
      log: () => {},
      reportError: () => {},
    });
    try {
      expect(config.restrict_to_project_root).toBe(true);
    } finally {
      reload.stop();
    }
  });
});

describe("watchAftConfigFiles registration and retries", () => {
  test("a directory replaced while its watch is being set up is watched again", async () => {
    const dir = tempDir();
    const configDir = join(dir, ".cortexkit");
    const file = join(configDir, "aft.jsonc");
    mkdirSync(configDir);
    writeFileSync(file, "{}");
    const watched: string[] = [];
    let replaced = false;
    const fakeWatch = ((path: string) => {
      watched.push(path);
      return { close: () => {}, on: () => {} };
    }) as unknown as typeof import("node:fs").watch;
    const stop = watchAftConfigFiles({
      paths: [file],
      onChange: () => undefined,
      watchImpl: fakeWatch,
      beforeWatchForTest: (target) => {
        if (replaced || target !== configDir) return;
        replaced = true;
        // Replace the directory between the identity read and the watch.
        rmSync(configDir, { recursive: true, force: true });
        mkdirSync(configDir);
        writeFileSync(file, "{}");
      },
    });
    try {
      await waitUntil(
        () => watched.filter((path) => path === configDir).length >= 2,
        "the replaced directory was watched again",
        4_000,
      );
    } finally {
      stop();
    }
  });

  test("a rejected text is checked again without another event", async () => {
    const dir = tempDir();
    const file = join(dir, "aft.jsonc");
    writeFileSync(file, "{}");
    const results = [false, true];
    let calls = 0;
    const stop = watchAftConfigFiles({
      paths: [file],
      debounceMs: 20,
      onChange: () => results[calls++] ?? true,
    });
    try {
      // Let the watch settle; an event right after it starts can be missed.
      await new Promise((resolve) => setTimeout(resolve, 300));
      writeFileSync(file, '{ "restrict_to_project_root": true }');
      await waitUntil(() => calls >= 2, "the rejected text was retried", 4_000);
    } finally {
      stop();
    }
  });
});
