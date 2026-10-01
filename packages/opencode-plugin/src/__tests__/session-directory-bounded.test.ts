import { afterEach, describe, expect, spyOn, test } from "bun:test";
import { mkdirSync, mkdtempSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Effect } from "effect";
import {
  _resetSessionDirectoryCacheForTest,
  getSessionDirectory,
  getSessionDirectoryCached,
  warmSessionDirectory,
} from "../shared/session-directory.js";
import { _resetSubagentCacheForTest } from "../shared/subagent-detect.js";
import { callBridge } from "../tools/_shared.js";
import type { PluginContext } from "../types.js";

const tempRoots: string[] = [];

afterEach(() => {
  for (const root of tempRoots.splice(0)) rmSync(root, { recursive: true, force: true });
  _resetSessionDirectoryCacheForTest();
  _resetSubagentCacheForTest();
});

for (const version of [1, 2]) {
  function client(get: () => Promise<{ directory: string }>) {
    return version === 1
      ? { session: { get } }
      : {
          location: { directory: "/call-directory" },
          session: {
            get: () => Effect.promise(get).pipe(Effect.map((session) => ({ location: session }))),
          },
        };
  }

  describe(`OpenCode ${version} bounded session directory`, () => {
    test("concurrent first calls share one host lookup", async () => {
      let calls = 0;
      let release!: (session: { directory: string }) => void;
      const host = client(() => {
        calls++;
        return new Promise((resolve) => {
          release = resolve;
        });
      });
      const first = getSessionDirectory(host, "concurrent", "/first");
      const second = getSessionDirectory(host, "concurrent", "/second");
      release({ directory: "/stored" });
      expect(await Promise.all([first, second])).toEqual(["/stored", "/stored"]);
      expect(calls).toBe(1);
    });

    test("failure suppresses lookups for 30 seconds then retries, including warmup", async () => {
      let calls = 0;
      let time = Date.now();
      const clock = spyOn(Date, "now").mockImplementation(() => time);
      try {
        const host = client(async () => {
          calls++;
          if (calls === 1) throw new Error("host unavailable");
          return { directory: "/recovered" };
        });
        expect(await getSessionDirectory(host, "retry", "/first")).toBeNull();
        time += 29_999;
        expect(await getSessionDirectory(host, "retry", "/second")).toBeNull();
        expect(calls).toBe(1);
        time++;
        expect(getSessionDirectoryCached("retry")).toBeUndefined();
        warmSessionDirectory(host, "retry", "/third");
        expect(await getSessionDirectory(host, "retry", "/third")).toBe("/recovered");
        expect(calls).toBe(2);
      } finally {
        clock.mockRestore();
      }
    });

    test("a timed-out lookup cools down, retries, and ignores its late answer", async () => {
      let calls = 0;
      let release!: (session: { directory: string }) => void;
      const host = client(() => {
        calls++;
        return calls === 1
          ? new Promise((resolve) => {
              release = resolve;
            })
          : Promise.resolve({ directory: "/retried" });
      });
      expect(await getSessionDirectory(host, "late", "/fallback")).toBeNull();
      expect(await getSessionDirectory(host, "late", "/other")).toBeNull();
      expect(calls).toBe(1);
      const clock = spyOn(Date, "now").mockReturnValue(Date.now() + 30_001);
      try {
        expect(await getSessionDirectory(host, "late", "/fallback")).toBe("/retried");
        release({ directory: "/obsolete" });
        await new Promise((resolve) => setTimeout(resolve, 10));
        expect(getSessionDirectoryCached("late")).toBe("/retried");
        expect(calls).toBe(2);
      } finally {
        clock.mockRestore();
      }
    });

    test("a host that never resolves lets callBridge use each call's directory within one second", async () => {
      const root = realpathSync(mkdtempSync(join(tmpdir(), "aft-directory-timeout-")));
      tempRoots.push(root);
      const firstDirectory = join(root, "first");
      const secondDirectory = join(root, "second");
      mkdirSync(firstDirectory);
      mkdirSync(secondDirectory);
      const roots: string[] = [];
      const host = client(() => new Promise(() => {}));
      const ctx = {
        client: host,
        pool: {
          getBridge: (root: string) => {
            roots.push(root);
            return { send: async () => ({ success: true }) };
          },
        },
        config: {},
        storageDir: "/isolated/storage",
      } as unknown as PluginContext;
      const runtime = {
        sessionID: "hang",
        directory: firstDirectory,
        worktree: root,
        directoryIsSessionRoot: version === 2,
      };
      const started = performance.now();
      await callBridge(ctx, runtime, "read", {});
      expect(performance.now() - started).toBeLessThan(1_500);
      expect(roots).toEqual([firstDirectory]);
      await callBridge(ctx, { ...runtime, directory: secondDirectory }, "read", {});
      expect(roots).toEqual([firstDirectory, secondDirectory]);
    }, 4_000);
  });
}
