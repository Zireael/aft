import { expect, test } from "bun:test";
import { Effect } from "effect";

import { currentSessionVisionCapability, rememberReadModel } from "../shared/read-vision.js";
import { bootV2Runtime } from "./helpers/v2-runtime.js";

test("V2 execute.before normalizes path aliases before the registered schema", async () => {
  const runtime = await bootV2Runtime();
  const before = runtime.toolHooks.get("execute.before");
  expect(before).toBeDefined();
  for (const name of ["read", "write", "edit"]) {
    const input =
      name === "write"
        ? { filePath: "file.ts", content: "new" }
        : name === "edit"
          ? { filePath: "file.ts", oldString: "old", newString: "new" }
          : { filePath: "file.ts" };
    const event = { tool: name, input };
    await Effect.runPromise(before!(event));
    expect(event.input).toHaveProperty("path", "file.ts");
    expect(runtime.tools.get(name).input.safeParse(event.input).success).toBe(true);
  }
  const foreign = { tool: "foreign", input: { filePath: "keep" } };
  await Effect.runPromise(before!(foreign));
  expect(foreign.input).toEqual({ filePath: "keep" });
});

test("V2 execute.after appends the conflicts hint only to completed bash text", async () => {
  const runtime = await bootV2Runtime();
  const after = runtime.toolHooks.get("execute.after");
  expect(after).toBeDefined();
  const text =
    "CONFLICT (content): file.ts\nAutomatic merge failed; fix conflicts and then commit the result.";
  const event = {
    tool: "bash",
    status: "completed",
    result: { content: [{ type: "text", text }], metadata: { exit: 1 } },
  };
  await Effect.runPromise(after!(event));
  expect(event.result.content[0]?.text).toContain("[Hint] Use aft_conflicts");
  expect(event.result.metadata).toEqual({ exit: 1 });
  const failure = { tool: "bash", status: "error", error: { message: text } };
  await Effect.runPromise(after!(failure));
  expect(failure.error.message).toBe(text);
});

test("V2 context model recording follows model switches for image reads", async () => {
  const runtime = await bootV2Runtime();
  const context = runtime.sessionHooks.get("context");
  expect(context).toBeDefined();
  for (const [id, expected] of [
    ["vision", true],
    ["text", false],
  ] as const) {
    await Effect.runPromise(
      context!({ sessionID: "s", model: { providerID: "p", id }, system: [] }),
    );
    expect(await currentSessionVisionCapability(runtime.context, "s")).toBe(expected);
  }
});

test("V2 vision lookup runs provider.list and reads native capabilities", async () => {
  const runtime = await bootV2Runtime();
  rememberReadModel(runtime.context, "s", { providerID: "p", modelID: "vision" });
  expect(await currentSessionVisionCapability(runtime.context, "s")).toBe(true);
});
