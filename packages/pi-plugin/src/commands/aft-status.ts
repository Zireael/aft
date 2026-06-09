/**
 * /aft-status — show AFT status (version, indexes, LSP, storage).
 *
 * Interactive mode opens a custom overlay dialog (see ./dialogs/status-dialog
 * for the Component implementation). The dialog refreshes every 1.5s so
 * index status transitions surface live. Non-UI mode (print / RPC) falls
 * back to a notification with a plain-text snapshot.
 */

import type { ExtensionAPI, ExtensionCommandContext } from "@earendil-works/pi-coding-agent";
import { showAftStatusDialog } from "../dialogs/status-dialog.js";
import { coerceAftStatus, formatStatusDialogMessage } from "../shared/status.js";
import { bridgeFor, callBridge, resolveSessionId } from "../tools/_shared.js";
import type { PluginContext } from "../types.js";

export function registerStatusCommand(pi: ExtensionAPI, ctx: PluginContext): void {
  pi.registerCommand("aft-status", {
    description: "Show AFT plugin status (search/semantic indexes, LSP, storage)",
    handler: async (_args: string, extCtx: ExtensionCommandContext) => {
      try {
        if (extCtx.hasUI) {
          await showAftStatusDialog(pi, extCtx, ctx);
          return;
        }
        // Non-UI mode — return a one-shot plain-text snapshot via notify.
        const bridge = bridgeFor(ctx, extCtx.cwd);
        const sessionId = resolveSessionId(extCtx);
        const cached = bridge.getCachedStatus();
        const cachedSession = (cached as Record<string, unknown> | null)?.session as
          | Record<string, unknown>
          | undefined;
        const cachedSessionId = cachedSession?.id as string | undefined;
        const cacheUsable =
          cached !== null && cachedSessionId !== undefined && cachedSessionId === sessionId;
        const response = cacheUsable
          ? { success: true, ...cached }
          : await callBridge(bridge, "status", {}, extCtx);
        if (!cacheUsable) {
          bridge.cacheStatusSnapshot(response);
        }
        const snapshot = coerceAftStatus(response);
        const text = formatStatusDialogMessage(snapshot);
        extCtx.ui.notify(text, "info");
      } catch (err) {
        const message = `AFT status failed: ${err instanceof Error ? err.message : String(err)}`;
        // Both UI and non-UI modes have access to ui.notify on Pi — UI mode
        // surfaces it as a transient toast, non-UI mode (print / RPC) routes
        // it through Pi's structured output. console.error is only the
        // last-resort fallback if notify itself throws (e.g. a malformed
        // ExtensionCommandContext in tests).
        try {
          extCtx.ui.notify(message, "error");
        } catch {
          console.error(`[aft-plugin] ${message}`);
        }
      }
    },
  });
}
