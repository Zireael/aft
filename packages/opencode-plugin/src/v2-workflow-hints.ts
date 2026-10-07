import type { SessionDomain } from "@opencode/plugin/effect/session";
import { Effect } from "effect";

import { log } from "./logger.js";

/** V2 consumes session system hooks, not V1's chat.system.transform. */
export function registerV2WorkflowHints(context: unknown, hintsBlock: string | null) {
  const host = context as { session?: SessionDomain };
  return Effect.gen(function* () {
    if (!hintsBlock || typeof host.session?.hook !== "function") return false;
    for (const name of ["context", "compaction"] as const) {
      yield* host.session.hook(name, (event) =>
        Effect.sync(() => {
          // Reuse the exact V1 block: changing bytes changes the prompt cache key.
          event.system.push({ type: "text", text: hintsBlock });
        }),
      );
    }
    log(`Workflow hints injected (${hintsBlock.length} chars)`);
    return true;
  });
}
