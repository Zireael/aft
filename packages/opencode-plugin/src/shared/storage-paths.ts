import { homedir } from "node:os";
import { join } from "node:path";

function homeDir(): string {
  if (process.platform === "win32") return process.env.USERPROFILE || process.env.HOME || homedir();
  return process.env.HOME || homedir();
}

function dataHome(): string {
  // Keep this in sync with the bridge package's storage migration helper,
  // but do not import that package from TUI code: its public barrel also
  // exports URL-fetch helpers unsuitable for Bun's TUI runtime.
  if (process.env.XDG_DATA_HOME) return process.env.XDG_DATA_HOME;
  if (process.platform === "win32") {
    return process.env.LOCALAPPDATA || process.env.APPDATA || join(homeDir(), "AppData", "Local");
  }
  return join(homeDir(), ".local", "share");
}

export function resolveCortexKitStorageRoot(): string {
  return join(dataHome(), "cortexkit", "aft");
}
