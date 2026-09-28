import { afterEach, describe, expect, mock, spyOn, test } from "bun:test";
import * as fs from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

const debugMock = mock(() => {});

mock.module("../../logger.js", () => ({
  log: mock(() => {}),
  debug: debugMock,
  warn: mock(() => {}),
  error: mock(() => {}),
}));

let importCounter = 0;

function freshCheckerImport() {
  return import(`./checker.ts?test=${importCounter++}`);
}

async function withoutNpm(
  run: (fixture: {
    root: string;
    directory: string;
    home: string;
    fetchMock: ReturnType<typeof mock<typeof fetch>>;
  }) => Promise<void>,
) {
  const root = fs.mkdtempSync(join(tmpdir(), "aft-no-npm-"));
  const home = join(root, "home");
  const directory = join(root, "project", "nested");
  fs.mkdirSync(home, { recursive: true });
  fs.mkdirSync(directory, { recursive: true });
  const keys = new Set([
    "PATH",
    "HOME",
    "USERPROFILE",
    "AFT_REGISTRY_HOST",
    ...Object.keys(process.env).filter((key) => key.toLowerCase().startsWith("npm_config_")),
  ]);
  const saved = new Map([...keys].map((key) => [key, process.env[key]]));
  for (const key of keys) delete process.env[key];
  process.env.PATH = "";
  process.env.HOME = home;
  process.env.USERPROFILE = home;
  const originalFetch = globalThis.fetch;
  const fetchMock = mock<typeof fetch>(async () =>
    Response.json({ "dist-tags": { latest: "9.0.0" } }),
  );
  globalThis.fetch = fetchMock;
  debugMock.mockClear();
  try {
    await run({ root, directory, home, fetchMock });
  } finally {
    globalThis.fetch = originalFetch;
    for (const key of Object.keys(process.env)) {
      if (key.toLowerCase().startsWith("npm_config_")) delete process.env[key];
    }
    for (const [key, value] of saved) {
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
    fs.rmSync(root, { recursive: true, force: true });
  }
}

afterEach(() => {
  mock.restore();
});

describe("auto-update-checker/checker", () => {
  describe("extractChannel", () => {
    test("returns latest for null, empty, and normal semver", async () => {
      const { extractChannel } = await freshCheckerImport();

      expect(extractChannel(null)).toBe("latest");
      expect(extractChannel("")).toBe("latest");
      expect(extractChannel("1.0.0")).toBe("latest");
    });

    test("keeps dist-tags and extracts common prerelease channels", async () => {
      const { extractChannel } = await freshCheckerImport();

      expect(extractChannel("beta")).toBe("beta");
      expect(extractChannel("next")).toBe("next");
      expect(extractChannel("1.0.0-alpha.1")).toBe("alpha");
      expect(extractChannel("2.3.4-beta.5")).toBe("beta");
      expect(extractChannel("0.1.0-rc.1")).toBe("rc");
      expect(extractChannel("1.0.0-canary.0")).toBe("canary");
    });
  });

  describe("findPluginEntry", () => {
    test("detects bare and @latest entries as unpinned", async () => {
      const existsSpy = spyOn(fs, "existsSync").mockImplementation((p: fs.PathLike) =>
        String(p).includes("opencode.json"),
      );
      const readSpy = spyOn(fs, "readFileSync").mockReturnValue(
        JSON.stringify({ plugin: ["@cortexkit/aft-opencode"] }),
      );
      const { findPluginEntry } = await freshCheckerImport();

      expect(findPluginEntry("/test")).toEqual({
        entry: "@cortexkit/aft-opencode",
        isPinned: false,
        pinnedVersion: null,
        configPath: "/test/.opencode/opencode.json",
      });

      readSpy.mockReturnValue(JSON.stringify({ plugin: ["@cortexkit/aft-opencode@latest"] }));
      expect(findPluginEntry("/test")?.isPinned).toBe(false);

      existsSpy.mockRestore();
      readSpy.mockRestore();
    });

    test("detects pinned tuple entries and ignores other scoped packages", async () => {
      const existsSpy = spyOn(fs, "existsSync").mockImplementation((p: fs.PathLike) =>
        String(p).includes("opencode.json"),
      );
      const readSpy = spyOn(fs, "readFileSync").mockReturnValue(
        JSON.stringify({
          plugin: ["@cortexkit/other@1.0.0", ["@cortexkit/aft-opencode@0.17.1", {}]],
        }),
      );
      const { findPluginEntry } = await freshCheckerImport();

      const entry = findPluginEntry("/test");
      expect(entry?.entry).toBe("@cortexkit/aft-opencode@0.17.1");
      expect(entry?.isPinned).toBe(true);
      expect(entry?.pinnedVersion).toBe("0.17.1");

      existsSpy.mockRestore();
      readSpy.mockRestore();
    });
  });

  describe("getLocalDevVersion", () => {
    test("returns null when no local plugin path is configured", async () => {
      const existsSpy = spyOn(fs, "existsSync").mockReturnValue(false);
      const { getLocalDevVersion } = await freshCheckerImport();

      expect(getLocalDevVersion("/test")).toBeNull();

      existsSpy.mockRestore();
    });

    test("returns version from a configured file:// local package", async () => {
      const existsSpy = spyOn(fs, "existsSync").mockImplementation((p: fs.PathLike) => {
        const value = String(p);
        return value.includes("opencode.json") || value === "/dev/aft/package.json";
      });
      const statSpy = spyOn(fs, "statSync").mockImplementation(
        () => ({ isDirectory: () => true }) as fs.Stats,
      );
      const readSpy = spyOn(fs, "readFileSync").mockImplementation((p: fs.PathOrFileDescriptor) => {
        const value = String(p);
        if (value.includes("opencode.json")) {
          return JSON.stringify({ plugin: ["file:///dev/aft"] });
        }
        if (value === "/dev/aft/package.json") {
          return JSON.stringify({ name: "@cortexkit/aft-opencode", version: "1.2.3-dev" });
        }
        return "";
      });
      const { getLocalDevVersion } = await freshCheckerImport();

      expect(getLocalDevVersion("/test")).toBe("1.2.3-dev");

      existsSpy.mockRestore();
      statSpy.mockRestore();
      readSpy.mockRestore();
    });
  });

  describe("getCachedVersion and updatePinnedVersion", () => {
    test("reads cached version from OpenCode's scoped package cache layout", async () => {
      const packagePath = `${homedir()}/.cache/opencode/packages/@cortexkit/aft-opencode@latest/node_modules/@cortexkit/aft-opencode/package.json`;
      const existsSpy = spyOn(fs, "existsSync").mockImplementation(
        (p: fs.PathLike) => String(p) === packagePath,
      );
      const readSpy = spyOn(fs, "readFileSync").mockReturnValue(
        JSON.stringify({ name: "@cortexkit/aft-opencode", version: "0.17.2" }),
      );
      const { getCachedVersion } = await freshCheckerImport();

      expect(getCachedVersion("@cortexkit/aft-opencode@latest")).toBe("0.17.2");

      existsSpy.mockRestore();
      readSpy.mockRestore();
    });

    test("updates exact quoted pinned entry while preserving surrounding JSONC", async () => {
      const existsSpy = spyOn(fs, "existsSync").mockReturnValue(true);
      const readSpy = spyOn(fs, "readFileSync").mockReturnValue(
        '{\n  // plugins\n  "plugin": ["@cortexkit/aft-opencode@0.17.1"]\n}',
      );
      const writes: string[] = [];
      const writeSpy = spyOn(fs, "writeFileSync").mockImplementation(
        (_path: fs.PathOrFileDescriptor, data: string | NodeJS.ArrayBufferView) => {
          writes.push(String(data));
        },
      );
      const { updatePinnedVersion } = await freshCheckerImport();

      expect(
        updatePinnedVersion("/config/opencode.jsonc", "@cortexkit/aft-opencode@0.17.1", "0.17.2"),
      ).toBe(true);
      expect(writes[0]).toContain('"@cortexkit/aft-opencode@0.17.2"');
      expect(writes[0]).toContain("// plugins");

      existsSpy.mockRestore();
      readSpy.mockRestore();
      writeSpy.mockRestore();
    });
  });

  describe("getLatestVersion", () => {
    test("no npm uses ancestor npmrc scoped registry with environment expansion", async () => {
      await withoutNpm(async ({ root, directory, fetchMock }) => {
        process.env.AFT_REGISTRY_HOST = "mirror.example.test";
        process.env.NPM_CONFIG_REGISTRY = "https://default.example.test/";
        fs.writeFileSync(
          join(root, "project", ".npmrc"),
          "# registry settings\n; ignored\n@cortexkit:registry = https://${AFT_REGISTRY_HOST}/npm/\n",
        );
        const { getLatestVersion } = await freshCheckerImport();
        expect(await getLatestVersion("latest", { directory })).toBe("9.0.0");
        expect(fetchMock.mock.calls[0]?.[0]).toBe(
          "https://mirror.example.test/npm/%40cortexkit/aft-opencode",
        );
      });
    });

    test("no npm and no registry configuration uses npmjs", async () => {
      await withoutNpm(async ({ directory, fetchMock }) => {
        const { getLatestVersion } = await freshCheckerImport();
        expect(await getLatestVersion("latest", { directory })).toBe("9.0.0");
        expect(fetchMock.mock.calls[0]?.[0]).toBe(
          "https://registry.npmjs.org/%40cortexkit/aft-opencode",
        );
        expect(debugMock).not.toHaveBeenCalled();
      });
    });

    test("no npm and unreadable npmrc skips the query and logs once", async () => {
      await withoutNpm(async ({ directory, fetchMock }) => {
        fs.mkdirSync(join(directory, ".npmrc"));
        const { getLatestVersion } = await freshCheckerImport();
        expect(await getLatestVersion("latest", { directory })).toBeNull();
        expect(await getLatestVersion("latest", { directory })).toBeNull();
        expect(fetchMock).not.toHaveBeenCalled();
        expect(debugMock).toHaveBeenCalledTimes(1);
      });
    });
    test("no npm honors environment, project and home registry precedence", async () => {
      for (const mode of ["home", "project", "upper-default", "lower-default", "scope"] as const) {
        await withoutNpm(async ({ directory, home, fetchMock }) => {
          fs.writeFileSync(join(home, ".npmrc"), "registry=https://home.example.test/\n");
          if (mode !== "home")
            fs.writeFileSync(join(directory, ".npmrc"), "registry=https://project.example.test/\n");
          if (mode === "upper-default")
            process.env.NPM_CONFIG_REGISTRY = "https://upper-default.example.test/";
          if (mode === "lower-default")
            process.env.npm_config_registry = "https://lower-default.example.test/";
          if (mode === "scope") {
            process.env["npm_config_@cortexkit:registry"] = "https://scope.example.test/";
            process.env.NPM_CONFIG_REGISTRY = "https://ignored.example.test/";
            fs.writeFileSync(
              join(directory, ".npmrc"),
              "@cortexkit:registry=https://ignored-project.example.test/\n",
            );
          }
          const { getLatestVersion } = await freshCheckerImport();
          expect(await getLatestVersion("latest", { directory })).toBe("9.0.0");
          expect(fetchMock.mock.calls[0]?.[0]).toBe(
            `https://${mode}.example.test/%40cortexkit/aft-opencode`,
          );
        });
      }
    });

    test("no npm does not fall back to public for an invalid configured registry", async () => {
      await withoutNpm(async ({ directory, fetchMock }) => {
        process.env.npm_config_registry = "${UNSET_AFT_REGISTRY_HOST}/npm";
        const { getLatestVersion } = await freshCheckerImport();
        expect(await getLatestVersion("latest", { directory })).toBeNull();
        expect(await getLatestVersion("latest", { directory })).toBeNull();
        expect(fetchMock).not.toHaveBeenCalled();
        expect(debugMock).toHaveBeenCalledTimes(1);
      });
    });

    test("resolves scoped npmrc before environment default and caches per project", async () => {
      const directory = fs.mkdtempSync(join(tmpdir(), "aft-registry-"));
      const originalFetch = globalThis.fetch;
      const previous = process.env.NPM_CONFIG_REGISTRY;
      const fetchMock = mock(async (_input: string | URL | Request) =>
        Response.json({ "dist-tags": { latest: "9.0.0" } }),
      );
      globalThis.fetch = fetchMock;
      process.env.NPM_CONFIG_REGISTRY = "https://default.example.test/";
      try {
        fs.writeFileSync(join(directory, "package.json"), "{}");
        fs.writeFileSync(
          join(directory, ".npmrc"),
          "@cortexkit:registry=https://scoped.example.test/mirror/\n",
        );
        const { getLatestVersion } = await freshCheckerImport();
        expect(await getLatestVersion("latest", { directory, timeoutMs: 15000 })).toBe("9.0.0");
        fs.writeFileSync(
          join(directory, ".npmrc"),
          "@cortexkit:registry=https://changed.example.test/\n",
        );
        expect(await getLatestVersion("latest", { directory, timeoutMs: 15000 })).toBe("9.0.0");
        expect(fetchMock.mock.calls.map((call) => call[0])).toEqual([
          "https://scoped.example.test/mirror/%40cortexkit/aft-opencode",
          "https://scoped.example.test/mirror/%40cortexkit/aft-opencode",
        ]);
      } finally {
        globalThis.fetch = originalFetch;
        if (previous === undefined) delete process.env.NPM_CONFIG_REGISTRY;
        else process.env.NPM_CONFIG_REGISTRY = previous;
        fs.rmSync(directory, { recursive: true, force: true });
      }
    });
    test("uses the default registry when the scope is unset", async () => {
      const directory = fs.mkdtempSync(join(tmpdir(), "aft-default-registry-"));
      const originalFetch = globalThis.fetch;
      const fetchMock = mock(async (_input: string | URL | Request) =>
        Response.json({ "dist-tags": { latest: "9.0.0" } }),
      );
      globalThis.fetch = fetchMock;
      try {
        fs.writeFileSync(join(directory, "package.json"), "{}");
        fs.writeFileSync(
          join(directory, ".npmrc"),
          "@cortexkit:registry=undefined\nregistry=https://default.example.test/\n",
        );
        const { getLatestVersion } = await freshCheckerImport();
        expect(await getLatestVersion("latest", { directory, timeoutMs: 15000 })).toBe("9.0.0");
        expect(fetchMock.mock.calls[0]?.[0]).toBe(
          "https://default.example.test/%40cortexkit/aft-opencode",
        );
        expect(fetchMock).toHaveBeenCalledTimes(1);
      } finally {
        globalThis.fetch = originalFetch;
        fs.rmSync(directory, { recursive: true, force: true });
      }
    });
    test("fetches channel dist-tag from npm registry package envelope", async () => {
      const fetchMock = mock(async () =>
        Response.json({ "dist-tags": { latest: "0.17.2", beta: "0.18.0-beta.1" } }),
      );
      const originalFetch = globalThis.fetch;
      globalThis.fetch = fetchMock;
      const { getLatestVersion } = await freshCheckerImport();

      expect(await getLatestVersion("beta", { registryUrl: "https://registry.example.test" })).toBe(
        "0.18.0-beta.1",
      );
      expect(fetchMock).toHaveBeenCalledWith(
        "https://registry.example.test/%40cortexkit/aft-opencode",
        expect.objectContaining({ headers: { Accept: "application/json" } }),
      );

      globalThis.fetch = originalFetch;
    });
  });
});
