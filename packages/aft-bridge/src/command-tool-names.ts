/**
 * The names an agent sees for AFT's command tool and its background-task
 * companions.
 *
 * OpenCode 1 and Pi register the command tool as `bash`. OpenCode 2 has its own
 * built-in command tool called `shell`, and AFT replaces it there under that
 * same name, so on that host the companions become `shell_status`,
 * `shell_watch`, `shell_kill` and `shell_write`. Every piece of text that tells
 * an agent which tool to call next has to use the names the agent was actually
 * given, otherwise it steers the agent at a tool that does not exist.
 *
 * These are display and registration names only. The bridge wire commands
 * (`bash`, `bash_status`, ...), the `bash.*` config keys, the `disabled_tools`
 * entries users write, and the `bash-<hex>` task IDs keep their canonical
 * spelling on every host.
 */
export type CommandToolName = "bash" | "shell";

export interface CommandToolNames {
  /** The command tool itself. */
  readonly command: CommandToolName;
  readonly status: string;
  readonly watch: string;
  readonly kill: string;
  readonly write: string;
}

/** Build the command tool name and its four companion names from one base name. */
export function commandToolNames(base: CommandToolName = "bash"): CommandToolNames {
  return {
    command: base,
    status: `${base}_status`,
    watch: `${base}_watch`,
    kill: `${base}_kill`,
    write: `${base}_write`,
  };
}

/** The names every host except OpenCode 2 registers. */
export const BASH_TOOL_NAMES: CommandToolNames = commandToolNames("bash");

/** The names AFT registers on OpenCode 2. */
export const SHELL_TOOL_NAMES: CommandToolNames = commandToolNames("shell");

/** True only for the two base names a host can register the command tool under. */
export function isCommandToolName(value: unknown): value is CommandToolName {
  return value === "bash" || value === "shell";
}
