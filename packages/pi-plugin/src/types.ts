import type { AftTransportPool } from "@cortexkit/aft-bridge";
import type { AftConfig } from "./config.js";
import type { OmpInternalUrlRouter } from "./omp-internal-urls.js";

/**
 * Shared context passed to every tool wrapper.
 * Bundles the bridge pool, the resolved AFT config, and the storage dir.
 *
 * Note: session ID is resolved per tool call from Pi's `ExtensionContext`
 * (`sessionManager.getSessionId()`) rather than stored here, so that
 * `/new`, `/fork`, and `/resume` each scope their own undo/checkpoint
 * state in AFT.
 */
export interface PluginContext {
  pool: AftTransportPool;
  config: AftConfig;
  /** OMP's process-global internal URL router, absent on upstream Pi. */
  ompRouter?: OmpInternalUrlRouter;
  /** Whether hashline edit/read mode is active for this plugin registration. */
  hashlineEffective?: boolean;
  /** Absolute path to AFT's data storage dir (e.g. ~/.local/share/cortexkit/aft). */
  storageDir: string;
  /**
   * Starts the work the extension defers until a session needs AFT (LSP
   * discovery and installs, ONNX Runtime preparation, the warmup bridge).
   * Idempotent; a no-op once the host has shut the session down.
   */
  startSessionWork?: () => void;
}
