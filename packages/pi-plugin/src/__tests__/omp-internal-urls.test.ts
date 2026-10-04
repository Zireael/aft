/// <reference path="../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";
import type { ExtensionContext } from "@earendil-works/pi-coding-agent";
import type { TSchema } from "typebox";
import { Value } from "typebox/value";
import { registerPiToolSurface, resolvePiToolSurface } from "../tool-registration.js";
import { makeContext, mockTheme, renderToString } from "./render-test-helpers.js";
import {
  executeTool,
  makeExtContext,
  makeMockApi,
  makeMockBridge,
  makePluginContext,
} from "./tool-test-utils.js";

// OMP 18.6.0 registers all these schemes, including mcp. MCP write refusal
// belongs to the native tool, not to the adapter's command-token predicate.
const schemes = new Set(
  "skill rule memory agent history artifact local proc cfg ssh security vault issue pr mcp omp xd attachment conflict".split(
    " ",
  ),
);
const writable = new Set("agent local proc cfg ssh vault xd conflict".split(" "));
const router = {
  normalize: (input: string) => input.replace(/^skill:\/(?!\/)/, "skill://"),
  canHandle(input: string) {
    return schemes.has(/^([a-z]+):\/\//i.exec(this.normalize(input))?.[1]?.toLowerCase() ?? "");
  },
  canResolve(input: string) {
    return this.canHandle(input) || /^(?:custom:\/\/|urn:)/.test(input);
  },
  writeTarget(input: string) {
    const scheme = /^([a-z]+):\/\//i.exec(this.normalize(input))?.[1]?.toLowerCase();
    return scheme && schemes.has(scheme)
      ? { spec: { write: writable.has(scheme) ? { via: "handler" } : undefined } }
      : undefined;
  },
};

function setup(harness: "omp" | "pi" = "omp", withRouter = true, hashline = false) {
  const { api, tools } = makeMockApi();
  const { bridge, calls } = makeMockBridge(() => ({ success: true, text: "AFT", output: "AFT" }));
  const config = { disabled_tools: [], bash: true, github: { read: true, write: true } };
  const ctx = makePluginContext(bridge, {
    config,
    hashlineEffective: hashline,
    ...{ ompRouter: withRouter ? router : undefined },
  });
  registerPiToolSurface(api, ctx, resolvePiToolSurface(config), harness);
  const invocations: unknown[] = [];
  const nativeResult = {
    content: [{ type: "text", text: "native status" }],
    details: { proc: { status: "killed" }, bg_completions: {}, diff: {} },
  };
  const extCtx = Object.assign(makeExtContext(), {
    invokeTool: async (params: unknown) => {
      invocations.push(params);
      return nativeResult;
    },
    hasUI: true,
    ui: { confirm: async () => true },
  }) as ExtensionContext;
  return { tools, calls, invocations, extCtx, nativeResult };
}

describe("OMP internal URL hand-back", () => {
  test("routing predicate delegates read without preparing or copying parameters", async () => {
    const s = setup();
    const params = { path: "agent://worker", offset: 0, opaque: "keep" };
    const prepare = (
      s.tools.get("read")! as unknown as { prepareArguments: (args: unknown) => unknown }
    ).prepareArguments;
    expect(prepare(params)).toBe(params);
    expect(await executeTool(s.tools.get("read")!, params, s.extCtx)).toBe(s.nativeResult);
    expect(s.invocations[0]).toBe(params);
    expect(s.calls).toHaveLength(0);
  });

  test("content-less proc kill passes the OMP write schema and delegates", async () => {
    const s = setup();
    const params = { path: "proc://x/kill" };
    expect(Value.Check(s.tools.get("write")!.parameters as TSchema, params)).toBe(true);
    await executeTool(s.tools.get("write")!, params, s.extCtx);
    expect(s.invocations[0]).toBe(params);
    expect(s.calls).toHaveLength(0);
  });

  test("xd and cfg writes and edits use invokeTool only", async () => {
    const s = setup();
    for (const path of ["xd://device", "cfg://settings"]) {
      for (const name of ["write", "edit"]) {
        const params = { path, content: "value" };
        await executeTool(s.tools.get(name)!, params, s.extCtx);
        expect(s.invocations.at(-1)).toBe(params);
      }
    }
    expect(s.calls).toHaveLength(0);
  });

  test("read-side router covers every registered scheme and opaque MCP fallback", async () => {
    const s = setup();
    for (const path of [...schemes]
      .map((scheme) => `${scheme}://x`)
      .concat(["custom://resource", "urn:example:doc"])) {
      await executeTool(s.tools.get("read")!, { path }, s.extCtx);
      await executeTool(s.tools.get("grep")!, { path, pattern: "x" }, s.extCtx);
    }
    expect(s.invocations).toHaveLength((schemes.size + 2) * 2);
    expect(s.calls).toHaveLength(0);
  });

  test("grep delegates mixed search paths as one unmodified call", async () => {
    const s = setup();
    for (const path of [["src", "local://*.md"], "src,local://*.md", "src;cfg://settings"]) {
      const params = { path, pattern: "x", skip: null, case: true };
      expect(Value.Check(s.tools.get("grep")!.parameters as TSchema, params)).toBe(true);
      await executeTool(s.tools.get("grep")!, params, s.extCtx);
      expect(s.invocations.at(-1)).toBe(params);
    }
    expect(s.calls).toHaveLength(0);
  });

  test("bash delegates native cwd and AFT workdir URLs", async () => {
    const s = setup();
    for (const key of ["cwd", "workdir"]) {
      const params = { command: "pwd", [key]: "skill://sample", timeout: 0.5 };
      expect(Value.Check(s.tools.get("bash")!.parameters as TSchema, params)).toBe(true);
      await executeTool(s.tools.get("bash")!, params, s.extCtx);
      expect(s.invocations.at(-1)).toBe(params);
    }
    expect(s.calls).toHaveLength(0);
  });

  test("bash tokenizer delegates quoted URLs, aliases and registered MCP", async () => {
    const s = setup();
    for (const command of [
      'cat "skill://sample"',
      "ls local://",
      "cat skill://sample",
      "cat 'skill:/sample'",
      "cat `skill://sample`",
      "cat mcp://x",
      "cmd > mcp://x",
      "cat\tlocal://x;pwd",
    ]) {
      const params = { command };
      await executeTool(s.tools.get("bash")!, params, s.extCtx);
      expect(s.invocations.at(-1)).toBe(params);
    }
    expect(s.calls).toHaveLength(0);
    expect((s.tools.get("bash") as unknown as { readsSkillUris?: boolean }).readsSkillUris).toBe(
      true,
    );
  });

  test("plain paths and http/file bash tokens never delegate", async () => {
    const s = setup();
    for (const [name, params] of [
      ["read", { path: "src/index.ts" }],
      ["write", { path: "src/index.ts", content: "text" }],
      ["edit", { path: "src/index.ts", appendContent: "text" }],
      ["grep", { path: "src", pattern: "text" }],
      ["bash", { command: "curl https://example.com" }],
      ["bash", { command: "curl http://example.com" }],
      ["bash", { command: "cat file:///tmp/x" }],
    ] as const)
      await executeTool(s.tools.get(name)!, params, s.extCtx);
    expect(s.invocations).toHaveLength(0);
    expect(s.calls).toHaveLength(7);
  });

  test("GitHub reads delegate but read-only OMP GitHub writes stay on AFT", async () => {
    const s = setup();
    await executeTool(s.tools.get("read")!, { path: "issue://5" }, s.extCtx);
    await executeTool(s.tools.get("write")!, { path: "issue://5", content: "comment" }, s.extCtx);
    await executeTool(
      s.tools.get("edit")!,
      { path: "pr://5/comments/1", edits: [{ oldString: "old", newString: "new" }] },
      s.extCtx,
    );
    expect(s.invocations).toEqual([{ path: "issue://5" }]);
    expect(s.calls.map((c) => c.params.name)).toEqual(["write", "edit", "edit"]);
  });

  test("schema audit accepts all native OMP edit modes, including hashline override", () => {
    for (const hashline of [false, true]) {
      const s = setup("omp", true, hashline);
      for (const params of [
        { path: "local://x", old_string: "old", new_string: "new", replace_all: true },
        {
          path: "local://x",
          edits: [{ op: "update", diff: "@@ -1 +1 @@\n-a\n+b", rename: "local://y" }],
        },
        { path: "local://x", edits: [] },
        { input: "native patch or hashline input" },
      ])
        expect(Value.Check(s.tools.get("edit")!.parameters as TSchema, params)).toBe(true);
    }
  });

  test("schema audit accepts native bash service and grep paging options", () => {
    const s = setup();
    expect(
      Value.Check(s.tools.get("bash")!.parameters as TSchema, {
        command: "pwd",
        cwd: "local://",
        timeout: 0.5,
        pty: true,
        async: true,
        name: "service",
        ready: { log: "ready", port: 3000, host: "localhost", timeout: 0.5 },
      }),
    ).toBe(true);
    expect(
      Value.Check(s.tools.get("grep")!.parameters as TSchema, {
        pattern: "x",
        path: "local://",
        case: false,
        gitignore: false,
        skip: null,
      }),
    ).toBe(true);
  });

  test("hashline override delegates path-bearing native edits before AFT preflight", async () => {
    const s = setup("omp", true, true);
    const params = { path: "local://x", old_string: "old", new_string: "new" };
    await executeTool(s.tools.get("edit")!, params, s.extCtx);
    expect(s.invocations[0]).toBe(params);
    expect(s.calls).toHaveLength(0);
  });

  test("OMP local paths retain AFT schema validation", async () => {
    const s = setup();
    await expect(executeTool(s.tools.get("write")!, { path: "x" }, s.extCtx)).rejects.toThrow();
    await expect(
      executeTool(s.tools.get("edit")!, { path: "x", edits: [] }, s.extCtx),
    ).rejects.toThrow();
    await expect(
      executeTool(s.tools.get("bash")!, { command: "true", timeout: 0.5 }, s.extCtx),
    ).rejects.toThrow();
    expect(s.calls).toHaveLength(0);
  });

  test("plain Pi and hosts missing invokeTool or router retain AFT behavior", async () => {
    for (const mode of ["pi", "no-invoke", "no-router"]) {
      const s = setup(mode === "pi" ? "pi" : "omp", mode !== "no-router");
      await executeTool(
        s.tools.get("read")!,
        { path: "agent://worker" },
        mode === "no-invoke" ? makeExtContext() : s.extCtx,
      );
      expect(s.invocations).toHaveLength(0);
      expect(s.calls).toHaveLength(1);
      if (mode !== "no-invoke") {
        expect(
          Value.Check(s.tools.get("write")!.parameters as TSchema, { path: "proc://x/kill" }),
        ).toBe(false);
        expect(
          (s.tools.get("bash") as unknown as { readsSkillUris?: boolean }).readsSkillUris,
        ).toBeUndefined();
      }
    }
  });

  test("upstream Pi retains all five parameter schemas and local call results", async () => {
    const pi = setup("pi");
    const withoutRouter = setup("omp", false);
    for (const name of ["read", "write", "edit", "grep", "bash"]) {
      expect(pi.tools.get(name)!.parameters).toEqual(withoutRouter.tools.get(name)!.parameters);
    }
    const params = { path: "x", content: "text" };
    const plainContext = makeExtContext();
    expect(await executeTool(pi.tools.get("write")!, params, plainContext)).toEqual(
      await executeTool(withoutRouter.tools.get("write")!, params, plainContext),
    );
    expect(pi.calls).toEqual(withoutRouter.calls);
    expect(pi.invocations).toHaveLength(0);
  });

  test("delegated renderers show path, status and native text without assuming AFT details", async () => {
    const s = setup();
    for (const name of ["read", "write", "edit", "grep", "bash"]) {
      const params =
        name === "bash"
          ? { command: 'cat "skill://sample"', cwd: "skill://sample" }
          : { path: "proc://x/kill", content: "value", pattern: "x" };
      const tool = s.tools.get(name)!;
      const result = await executeTool(tool, params, s.extCtx);
      const component = tool.renderResult!(
        result,
        { expanded: true },
        mockTheme,
        makeContext(params),
      ) as import("@earendil-works/pi-tui").Component;
      const rendered = renderToString(component);
      expect(rendered).toContain(name === "bash" ? "skill://sample" : "proc://x/kill");
      expect(rendered).toContain("OMP");
      expect(rendered).toContain("native status");
      const error = tool.renderResult!(
        { content: [], details: null, isError: true },
        {},
        mockTheme,
        makeContext(params, { isError: true }),
      ) as import("@earendil-works/pi-tui").Component;
      expect(renderToString(error)).toContain("error");
    }
  });
});
