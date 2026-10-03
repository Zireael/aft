import type { SessionContext, SessionDomain } from "@opencode/plugin/effect/session";
import type { ToolDomain } from "@opencode/plugin/effect/tool";
import { Effect, Schema } from "effect";

import { prepareOpenCodeArguments } from "./normalize-schemas.js";
import { maybeAppendConflictsHint } from "./shared/bash-hints.js";
import { rememberReadModel } from "./shared/read-vision.js";

// The host catches this tagged failure as a failed tool call, not a session
// defect. Use its public error shape without coupling to a second schema copy.
class ArgumentError extends Schema.TaggedError<ArgumentError>()("Tool.Error", {
  message: Schema.String,
}) {}

/** Match the host's GPT patch preference without touching tools AFT did not register. */
function gateEditingTools(event: SessionContext, registeredTools: ReadonlySet<string>): void {
  const id = event.model.id;
  const prefersPatch = id.includes("gpt-") && !id.includes("oss") && !id.includes("gpt-4");
  for (const name of prefersPatch ? ["edit", "write"] : ["apply_patch"]) {
    if (registeredTools.has(name)) delete event.tools[name];
  }
}

/** Port the V1 raw-argument, result-text and current-model seams to V2. */
export function registerV2ToolHooks(
  context: unknown,
  runtime: { hashlineEffective?: boolean },
  registeredTools: ReadonlySet<string>,
) {
  const host = context as { tool?: ToolDomain; session?: SessionDomain };
  return Effect.gen(function* () {
    if (typeof host.tool?.hook === "function") {
      yield* host.tool.hook("execute.before", (event) =>
        Effect.try({
          try: () => {
            if (!registeredTools.has(event.tool)) return;
            event.input = prepareOpenCodeArguments(event.tool, event.input, {
              hashlineEffective: runtime.hashlineEffective,
            });
          },
          // Reject before the host can strip unknown fields from the raw input.
          catch: (error) =>
            new ArgumentError({ message: error instanceof Error ? error.message : String(error) }),
        }),
      );
      yield* host.tool.hook("execute.after", (event) =>
        Effect.sync(() => {
          if (
            event.tool !== "bash" ||
            event.status !== "completed" ||
            !registeredTools.has("bash") ||
            !registeredTools.has("aft_conflicts")
          )
            return;
          const content = event.result.content;
          event.result = {
            ...event.result,
            content:
              typeof content === "string"
                ? maybeAppendConflictsHint(content)
                : content?.map((part) =>
                    part.type === "text"
                      ? { ...part, text: maybeAppendConflictsHint(part.text) }
                      : part,
                  ),
          };
        }),
      );
    }
    if (typeof host.session?.hook === "function") {
      // Each request starts with its own tool draft, including auxiliary requests.
      // Disabling the built-in patch plugin removes its gate, so AFT owns this one.
      for (const name of ["compaction", "generate"] as const) {
        yield* host.session.hook(name, (event) =>
          Effect.sync(() => gateEditingTools(event, registeredTools)),
        );
      }
      yield* host.session.hook("context", (event) =>
        Effect.sync(() => {
          gateEditingTools(event, registeredTools);
          rememberReadModel(context as object, event.sessionID, {
            providerID: event.model.providerID,
            modelID: event.model.id,
          });
        }),
      );
    }
  });
}
