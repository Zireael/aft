import assert from "node:assert/strict";
import { join } from "node:path";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { loadOmpInternalUrlRouter } from "../../omp-internal-urls.js";
import { registerPiToolSurface, resolvePiToolSurface } from "../../tool-registration.js";

// Real AFT registration, loaded through OMP's extension import resolver. A URL
// call reaching the filesystem bridge is a test failure, not a fallback success.
export default async function (api: ExtensionAPI): Promise<void> {
  const config = { disabled_tools: [], bash: true };
  const ompRouter = await loadOmpInternalUrlRouter(api);
  assert(ompRouter, "OMP router did not resolve through the optional host import");
  registerPiToolSurface(
    api,
    {
      config,
      ompRouter,
      storageDir: join(process.env.HOME!, "aft"),
      pool: {
        getBridge: () => {
          throw new Error("Internal URLs must never reach the AFT bridge");
        },
      } as any,
    },
    resolvePiToolSurface(config),
  );
}
