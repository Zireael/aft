import type { AftTransportPool, CommandToolName } from "@cortexkit/aft-bridge";
import type { PluginInput } from "@opencode-ai/plugin";
import type { AftConfig } from "./config.js";

interface ShellEnvPluginHost {
  trigger?: (
    name: "shell.env",
    context: { cwd: string; sessionID?: string; callID?: string },
    input: { env: Record<string, string> },
  ) => Promise<{ env?: Record<string, string> }> | { env?: Record<string, string> };
}

/**
 * Shared context passed to all tool factory functions.
 * Bundles the binary bridge, the OpenCode SDK client, and plugin config.
 */
export interface PluginContext {
  pool: AftTransportPool;
  client: PluginInput["client"];
  plugin?: ShellEnvPluginHost;
  config: AftConfig;
  /** Whether hashline edit/read mode is active for this plugin registration. */
  hashlineEffective?: boolean;
  /** Absolute path to AFT's storage dir (e.g. ~/.local/share/opencode/storage/plugin/aft) */
  storageDir: string;
  /** Returns false when user and project config merge to `enabled: false` for this project. */
  isProjectEnabled?: (projectRoot: string) => boolean;
  /**
   * The name the command tool is registered under: `shell` on OpenCode 2,
   * where AFT replaces the host's own shell tool, and `bash` (the default)
   * everywhere else. Tool descriptions and every hint that names the command
   * tool or its companions are rendered from it.
   */
  commandToolName?: CommandToolName;
}
