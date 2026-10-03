import { expect, spyOn, test } from "bun:test";
import { Effect } from "effect";

import * as logger from "../logger.js";
import { registerV2WorkflowHints } from "../v2-workflow-hints.js";
import { buildHintsForRegisteredTools } from "../workflow-hints.js";
import { bootV2Runtime } from "./helpers/v2-runtime.js";

test("V2 context and compaction inject byte-identical V1 workflow guidance after surface overrides", async () => {
  const runtime = await bootV2Runtime({
    edit_mode: "hashline",
    disabled_tools: ["edit", "aft_inspect"],
  });
  const expected = buildHintsForRegisteredTools(
    { edit_mode: "hashline" },
    new Set(runtime.tools.keys()),
    false,
  );
  expect(runtime.overrides.get("edit_slot_survives")).toBe(false);
  expect(expected).not.toContain("**Hashline edit tags**");
  expect(expected).not.toContain("**AFT status bar**");
  for (const name of ["context", "compaction"]) {
    const hook = runtime.sessionHooks.get(name);
    expect(hook).toBeDefined();
    const event = {
      sessionID: "s",
      model: { providerID: "p", id: "text" },
      system: [{ type: "text", text: "host system" }],
    };
    await Effect.runPromise(hook!(event));
    expect(event.system).toEqual([
      { type: "text", text: "host system" },
      { type: "text", text: expected },
    ]);
  }
});

test("V2 workflow injection is logged only when guidance hooks are registered", async () => {
  const log = spyOn(logger, "log");
  try {
    log.mockClear();
    await bootV2Runtime();
    const guidance = log.mock.calls.filter(([line]) =>
      String(line).startsWith("Workflow hints injected"),
    );
    expect(guidance).toHaveLength(1);
    log.mockClear();
    expect(await Effect.runPromise(registerV2WorkflowHints({}, "guidance"))).toBe(false);
    expect(log.mock.calls).toHaveLength(0);
    const rejected = await Effect.runPromiseExit(
      registerV2WorkflowHints(
        {
          session: { hook: () => Effect.die("registration refused") },
        },
        "guidance",
      ),
    );
    expect(rejected._tag).toBe("Failure");
    expect(log.mock.calls).toHaveLength(0);
    // No registered tool can support a hints section on this surface.
    await bootV2Runtime({
      disabled_tools: [
        "aft_outline",
        "aft_zoom",
        "aft_search",
        "aft_callgraph",
        "aft_inspect",
        "bash",
        "grep",
        "read",
      ],
    });
    expect(
      log.mock.calls.some(([line]) => String(line).startsWith("Workflow hints injected")),
    ).toBe(false);
  } finally {
    log.mockRestore();
  }
});
