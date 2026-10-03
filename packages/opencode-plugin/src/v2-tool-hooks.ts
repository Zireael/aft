import type { SessionDomain } from "@opencode/plugin/effect/session";
import type { ToolDomain } from "@opencode/plugin/effect/tool";
import { Effect } from "effect";

import { prepareOpenCodeArguments } from "./normalize-schemas.js";
import { maybeAppendConflictsHint } from "./shared/bash-hints.js";
import { rememberReadModel } from "./shared/read-vision.js";

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
        Effect.sync(() => {
          if (!registeredTools.has(event.tool)) return;
          try {
            event.input = prepareOpenCodeArguments(event.tool, event.input, {
              hashlineEffective: runtime.hashlineEffective,
            });
          } catch {
            // Invalid arguments still go through host schema validation and the
            // shared executor's strict preparation; never fail the whole session
            // with a defect from this compatibility-only hook.
          }
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
      yield* host.session.hook("context", (event) =>
        Effect.sync(() => {
          rememberReadModel(context as object, event.sessionID, {
            providerID: event.model.providerID,
            modelID: event.model.id,
          });
        }),
      );
    }
  });
}
