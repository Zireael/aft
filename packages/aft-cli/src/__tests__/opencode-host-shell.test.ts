/// <reference path="../bun-test.d.ts" />

/**
 * The OpenCode 2 host-shell offer: `aft setup` and `doctor --fix` can remove
 * OpenCode 2's built-in shell tool plugin (`-opencode.tool.shell` under
 * `plugins`) so AFT's `bash` is the only command tool the agent sees.
 */
import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { OpenCodeAdapter } from "../adapters/opencode.js";
import { hostShellDoctorLine, hostToolDoctorLine } from "../commands/doctor.js";
import {
  HOST_SHELL_QUESTION,
  offerHostShellDisable,
  offerHostToolDisable,
} from "../commands/setup.js";
import type { OpenCodeHostDetection } from "../setup/host-generation.js";
import {
  aftBashEnabled,
  aftHostReplacementEnabled,
  hostShellPluginDisabled,
  hostToolPluginDisabled,
  OPENCODE_HOST_SHELL_DISABLE_ENTRY,
  setHostShellPluginDisabled,
  setHostToolPluginDisabled,
} from "../setup/opencode-config.js";

function detection(status: "v1" | "v2"): OpenCodeHostDetection {
  return {
    status,
    generations: [status],
    evidence: [
      {
        generation: status,
        executable: status === "v2" ? "/fixture/opencode2" : "/fixture/opencode",
        version: null,
        runtime: "node",
        modernV1: status === "v1",
      },
    ],
  };
}

describe("the -opencode.tool.shell entry", () => {
  test("adding is idempotent and keeps every other entry in place", () => {
    const config: Record<string, unknown> = { plugins: ["a", "@cortexkit/aft-opencode@1.0.0"] };
    expect(setHostShellPluginDisabled(config, true)).toEqual({ changed: true });
    expect(config.plugins).toEqual([
      "a",
      "@cortexkit/aft-opencode@1.0.0",
      OPENCODE_HOST_SHELL_DISABLE_ENTRY,
    ]);
    expect(setHostShellPluginDisabled(config, true)).toEqual({ changed: false });
    expect(hostShellPluginDisabled(config)).toBe(true);
  });

  test("duplicates collapse to one and removal clears every copy", () => {
    const config: Record<string, unknown> = {
      plugins: [OPENCODE_HOST_SHELL_DISABLE_ENTRY, "a", OPENCODE_HOST_SHELL_DISABLE_ENTRY],
    };
    expect(setHostShellPluginDisabled(config, true)).toEqual({ changed: true });
    expect(config.plugins).toEqual([OPENCODE_HOST_SHELL_DISABLE_ENTRY, "a"]);
    expect(setHostShellPluginDisabled(config, false)).toEqual({ changed: true });
    expect(config.plugins).toEqual(["a"]);
    expect(setHostShellPluginDisabled(config, false)).toEqual({ changed: false });
  });

  test("removing from a config with no plugins list creates nothing", () => {
    const config: Record<string, unknown> = {};
    expect(setHostShellPluginDisabled(config, false)).toEqual({ changed: false });
    expect(config).toEqual({});
  });

  test("AFT's bash counts as enabled unless it is disabled as a tool or at runtime", () => {
    expect(aftBashEnabled(null)).toBe(true);
    expect(aftBashEnabled({})).toBe(true);
    expect(aftBashEnabled({ bash: true })).toBe(true);
    expect(aftBashEnabled({ bash: { compress: false } })).toBe(true);
    expect(aftBashEnabled({ bash: false })).toBe(false);
    expect(aftBashEnabled({ bash: { enabled: false } })).toBe(false);
    expect(aftBashEnabled({ disabled_tools: ["bash"] })).toBe(false);
    expect(aftBashEnabled({ enabled: false })).toBe(false);
  });
});

describe("the -opencode.tool.patch entry", () => {
  test("patch removal deduplicates without changing shell or other entries", () => {
    const config: Record<string, unknown> = {
      plugins: ["-opencode.tool.patch", "x", "-opencode.tool.shell", "-opencode.tool.patch"],
    };
    expect(setHostToolPluginDisabled(config, "patch", true)).toEqual({ changed: true });
    expect(config.plugins).toEqual(["-opencode.tool.patch", "x", "-opencode.tool.shell"]);
    expect(hostToolPluginDisabled(config, "patch")).toBe(true);
    expect(setHostToolPluginDisabled(config, "patch", true)).toEqual({ changed: false });
    expect(setHostToolPluginDisabled(config, "patch", false)).toEqual({ changed: true });
    expect(config.plugins).toEqual(["x", "-opencode.tool.shell"]);
    expect(hostToolPluginDisabled(config, "patch")).toBe(false);
    expect(setHostToolPluginDisabled(config, "patch", false)).toEqual({ changed: false });
    const empty = {};
    expect(setHostToolPluginDisabled(empty, "patch", false)).toEqual({ changed: false });
    expect(empty).toEqual({});
  });

  test("AFT apply_patch enabled state is independent of the bash runtime gate", () => {
    expect(aftHostReplacementEnabled(null, "patch")).toBe(true);
    expect(aftHostReplacementEnabled({ bash: false }, "patch")).toBe(true);
    expect(aftHostReplacementEnabled({ disabled_tools: ["edit"] }, "patch")).toBe(true);
    expect(aftHostReplacementEnabled({ disabled_tools: ["apply_patch"] }, "patch")).toBe(false);
    expect(aftHostReplacementEnabled({ enabled: false }, "patch")).toBe(false);
  });
});

describe("setup and doctor offer to disable OpenCode's own shell tool", () => {
  let root: string;
  const saved = { dir: process.env.OPENCODE_CONFIG_DIR, xdg: process.env.XDG_CONFIG_HOME };

  beforeEach(() => {
    root = mkdtempSync(join(tmpdir(), "aft-host-shell-"));
    process.env.OPENCODE_CONFIG_DIR = join(root, "opencode");
    process.env.XDG_CONFIG_HOME = join(root, "xdg");
  });
  afterEach(() => {
    rmSync(root, { recursive: true, force: true });
    if (saved.dir === undefined) delete process.env.OPENCODE_CONFIG_DIR;
    else process.env.OPENCODE_CONFIG_DIR = saved.dir;
    if (saved.xdg === undefined) delete process.env.XDG_CONFIG_HOME;
    else process.env.XDG_CONFIG_HOME = saved.xdg;
  });

  function adapter(status: "v1" | "v2", opencodeJson?: unknown, aftJsonc?: unknown) {
    const instance = new OpenCodeAdapter();
    instance.useHostDetection(detection(status));
    const paths = instance.detectConfigPaths();
    if (opencodeJson !== undefined) {
      Bun.spawnSync(["mkdir", "-p", paths.configDir]);
      writeFileSync(join(paths.configDir, "opencode.json"), JSON.stringify(opencodeJson));
    }
    if (aftJsonc !== undefined) {
      Bun.spawnSync(["mkdir", "-p", join(root, "xdg", "cortexkit")]);
      writeFileSync(paths.aftConfig, JSON.stringify(aftJsonc));
    }
    return instance;
  }

  function serverConfig(instance: OpenCodeAdapter): Record<string, unknown> {
    return JSON.parse(readFileSync(instance.detectConfigPaths().harnessConfig, "utf8"));
  }

  test("the prompt defaults to disabling when bash is enabled and writes the entry once", async () => {
    const instance = adapter("v2", { plugins: ["@cortexkit/aft-opencode@1.0.0"] });
    const asked: Array<[string, boolean]> = [];
    const confirmHostShell = async (message: string, defaultYes: boolean) => {
      asked.push([message, defaultYes]);
      return defaultYes;
    };

    expect(await offerHostShellDisable(instance, [], { interactive: true, confirmHostShell })).toBe(
      "ok",
    );
    expect(await offerHostShellDisable(instance, [], { interactive: true, confirmHostShell })).toBe(
      "ok",
    );

    expect(asked).toEqual([
      [HOST_SHELL_QUESTION, true],
      [HOST_SHELL_QUESTION, true],
    ]);
    expect(serverConfig(instance).plugins).toEqual([
      "@cortexkit/aft-opencode@1.0.0",
      OPENCODE_HOST_SHELL_DISABLE_ENTRY,
    ]);
    expect(instance.hostShellState()).toBe("disabled");
  });

  test("answering no removes the entry", async () => {
    const instance = adapter("v2", { plugins: ["x", OPENCODE_HOST_SHELL_DISABLE_ENTRY] });
    await offerHostShellDisable(instance, [], {
      interactive: true,
      confirmHostShell: async () => false,
    });
    expect(serverConfig(instance).plugins).toEqual(["x"]);
    expect(instance.hostShellState()).toBe("enabled");
  });

  test("with bash disabled the default is to keep the host's shell", async () => {
    const instance = adapter(
      "v2",
      { plugins: ["x", OPENCODE_HOST_SHELL_DISABLE_ENTRY] },
      { disabled_tools: ["bash"] },
    );
    const defaults: boolean[] = [];
    await offerHostShellDisable(instance, [], {
      interactive: true,
      confirmHostShell: async (_message, defaultYes) => {
        defaults.push(defaultYes);
        return defaultYes;
      },
    });
    expect(defaults).toEqual([false]);
    expect(serverConfig(instance).plugins).toEqual(["x"]);
  });

  test("--yes applies the default without asking", async () => {
    const instance = adapter("v2", { plugins: [] });
    await offerHostShellDisable(instance, ["--yes"], {
      interactive: true,
      confirmHostShell: async () => {
        throw new Error("must not prompt under --yes");
      },
    });
    expect(serverConfig(instance).plugins).toEqual([OPENCODE_HOST_SHELL_DISABLE_ENTRY]);
  });

  test("OpenCode 1 is never asked and its config is never touched", async () => {
    const original = { plugin: ["@cortexkit/aft-opencode@1.0.0"] };
    const instance = adapter("v1", original);
    const before = readFileSync(instance.detectConfigPaths().harnessConfig, "utf8");
    expect(
      await offerHostShellDisable(instance, [], {
        interactive: true,
        confirmHostShell: async () => {
          throw new Error("OpenCode 1 has no shell plugin to ask about");
        },
      }),
    ).toBe("skipped");
    expect(readFileSync(instance.detectConfigPaths().harnessConfig, "utf8")).toBe(before);
    expect(hostShellDoctorLine(instance)).toBeNull();
  });

  test("doctor reports whether the entry is set", () => {
    expect(hostShellDoctorLine(adapter("v2", { plugins: [] }))).toEqual({
      level: "warn",
      text: expect.stringContaining("OpenCode's own shell tool: enabled beside AFT's bash"),
    });
    expect(
      hostShellDoctorLine(adapter("v2", { plugins: [OPENCODE_HOST_SHELL_DISABLE_ENTRY] })),
    ).toEqual({
      level: "info",
      text: expect.stringContaining("OpenCode's own shell tool: disabled"),
    });
  });

  test("setup adds the literal patch removal entry once beside AFT apply_patch", async () => {
    const instance = adapter("v2", { plugins: ["x", "-opencode.tool.shell"] });
    const asked: Array<[string, boolean]> = [];
    for (let i = 0; i < 2; i++) {
      expect(
        await offerHostToolDisable(instance, "patch", [], {
          interactive: true,
          confirmHostTool: async (message, defaultYes) => {
            asked.push([message, defaultYes]);
            return defaultYes;
          },
        }),
      ).toBe("ok");
    }
    expect(asked).toEqual([
      ["OpenCode's own patch tool: disable it so agents use AFT's apply_patch?", true],
      ["OpenCode's own patch tool: disable it so agents use AFT's apply_patch?", true],
    ]);
    expect(serverConfig(instance).plugins).toEqual([
      "x",
      "-opencode.tool.shell",
      "-opencode.tool.patch",
    ]);
  });

  for (const [hostTool, aftTool] of [
    ["shell", "bash"],
    ["patch", "apply_patch"],
  ] as const) {
    test(`setup keeps host ${hostTool} by default with AFT ${aftTool} disabled`, async () => {
      const instance = adapter(
        "v2",
        { plugins: [`-opencode.tool.${hostTool}`] },
        {
          disabled_tools: [aftTool],
        },
      );
      const defaults: boolean[] = [];
      await offerHostToolDisable(instance, hostTool, [], {
        interactive: true,
        confirmHostTool: async (_message, defaultYes) => {
          defaults.push(defaultYes);
          return defaultYes;
        },
      });
      expect(defaults).toEqual([false]);
      expect(serverConfig(instance).plugins).toEqual([]);
      expect(hostToolDoctorLine(instance, hostTool)).toEqual({
        level: "info",
        text: `OpenCode's own ${hostTool} tool: enabled (AFT's ${aftTool} is disabled, so it stays)`,
      });
    });

    test(`doctor reports enabled and already disabled host ${hostTool}`, () => {
      expect(hostToolDoctorLine(adapter("v2", { plugins: [] }), hostTool)).toEqual({
        level: "warn",
        text: expect.stringContaining(
          `OpenCode's own ${hostTool} tool: enabled beside AFT's ${aftTool}`,
        ),
      });
      expect(
        hostToolDoctorLine(adapter("v2", { plugins: [`-opencode.tool.${hostTool}`] }), hostTool),
      ).toEqual({
        level: "info",
        text: `OpenCode's own ${hostTool} tool: disabled (-opencode.tool.${hostTool} is set); agents use AFT's ${aftTool}`,
      });
      expect(hostToolDoctorLine(adapter("v1", { plugin: [] }), hostTool)).toBeNull();
    });

    test(`setup skips host ${hostTool} on OpenCode 1`, async () => {
      const original = { plugin: ["x"] };
      const instance = adapter("v1", original);
      expect(await offerHostToolDisable(instance, hostTool, ["--yes"])).toBe("skipped");
      expect(serverConfig(instance)).toEqual(original);
    });
  }
});
