import { readdir, readFile, readlink } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { execFile } from "@cortexkit/aft-bridge";
import { OpenCode } from "@opencode/client";
import { headers } from "@opencode/client/service";

import { log, warn } from "../logger.js";
import type { V2PermissionClient, V2PermissionCreateInput } from "./v2.js";

/**
 * Why no prompt client could be built, as a stable name next to the sentence.
 *
 * - `server_url_invalid`: the user's `opencode.server_url` setting cannot be
 *   used as written (not an http(s) URL, or its password variable is unset).
 * - `server_url_unreachable`: the configured server did not answer, or refused
 *   AFT's credentials.
 * - `own_server_unaddressable`: no setting, and AFT could not find and
 *   authenticate to the OpenCode server it runs inside.
 */
export type V2PromptUnavailableReason =
  | "server_url_invalid"
  | "server_url_unreachable"
  | "own_server_unaddressable";

/**
 * The outcome of looking for the OpenCode server that can show a prompt.
 *
 * The failure branch carries what was actually observed rather than a verdict,
 * because it ends up in the sentence the user reads when a prompt could not be
 * raised, and that sentence has to say how to fix it.
 */
export type V2PromptChannelResult =
  | { readonly client: V2PermissionClient; readonly unavailable?: undefined }
  | {
      readonly client?: undefined;
      readonly unavailable: string;
      readonly reason?: V2PromptUnavailableReason;
    };

export interface V2PromptChannel {
  /** The client for the OpenCode server, or why none could be reached. */
  client(): Promise<V2PromptChannelResult>;
}

/** The user-tier `opencode` config block (see config.ts). */
export interface V2PromptServerSettings {
  readonly server_url?: string;
  readonly server_password_env?: string;
}

/** One TCP socket a process is listening on, as the operating system reports it. */
export interface V2ListeningSocket {
  /** Bind address: a concrete IP, or a wildcard such as `0.0.0.0`, `::` or `*`. */
  readonly address: string;
  readonly port: number;
}

/**
 * What discovery reads about the process AFT runs in.
 *
 * OpenCode 2 loads plugins inside its server process, so this process's own
 * pid, command line, environment and listening sockets are the server's.
 * Tests replace it.
 */
export interface V2ServerProcess {
  readonly pid: number;
  readonly argv: readonly string[];
  readonly env: Readonly<Record<string, string | undefined>>;
  /** Every OpenCode service registration file on this machine, by path. */
  registrations(): Promise<readonly { readonly path: string; readonly text: string }[]>;
  /**
   * The TCP ports this process listens on. Rejects when the operating system
   * could not be asked, which is different from an empty answer.
   */
  listeningSockets(): Promise<readonly V2ListeningSocket[]>;
  fetch(
    url: string,
    init: { headers: Record<string, string>; signal: AbortSignal },
  ): Promise<Response>;
}

export interface V2DiscoveryOptions {
  /** Bound on each health call. */
  readonly timeoutMs?: number;
}

/**
 * The username OpenCode 2's server checks Basic auth against.
 *
 * It is fixed: the server's auth layer (`server/auth.ts`, `ServerAuth.Config`)
 * always sets `username: "opencode"` and ignores OPENCODE_SERVER_USERNAME,
 * which only OpenCode 1 honored. Sending that variable's value would be refused.
 */
export const OPENCODE_BASIC_AUTH_USERNAME = "opencode";

const DEFAULT_PROBE_TIMEOUT_MS = 2_000;
/**
 * Limit on asking the operating system for this process's listening ports, so a
 * hung `lsof` or `netstat` cannot hold a permission prompt open.
 */
const LISTEN_LOOKUP_TIMEOUT_MS = 5_000;

/**
 * How to get out of the unaddressable state; appended to every reason that
 * leaves the user with nothing configured.
 */
const HOW_TO_FIX =
  'set "opencode.server_url" in your user AFT config to this server\'s URL, with ' +
  '"opencode.server_password_env" naming the environment variable that holds its password ' +
  "(for OpenChamber, the host and port given as OPENCODE_HOST / OPENCODE_PORT), or start " +
  "OpenCode with OPENCODE_SERVER_PASSWORD set so AFT can authenticate to it";

type ServiceEndpoint = {
  readonly url: string;
  readonly auth?: { readonly type: "basic"; readonly username: string; readonly password: string };
};

function detail(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/**
 * Adapt the generated OpenCode client to the two calls a permission prompt needs.
 *
 * `event.subscribe` is wrapped rather than used directly so the prompt code keeps
 * taking a subscription object it can hold open across the `permission.create`
 * call; the generated client hands back the async iterable immediately.
 */
function promptClient(endpoint: ServiceEndpoint): V2PermissionClient {
  const client = OpenCode.make({ baseUrl: endpoint.url, headers: headers(endpoint) });
  return {
    permission: {
      // The generated input types metadata as JSON values. AFT's request
      // metadata is assembled by the tools and is JSON-serialisable by
      // construction, so it is handed over as-is.
      create: (input: V2PermissionCreateInput) =>
        client.permission.create(input as Parameters<typeof client.permission.create>[0]),
    },
    event: {
      subscribe: async () => ({ stream: client.event.subscribe() }),
    },
  };
}

function basicAuth(password: string | undefined): ServiceEndpoint["auth"] {
  if (!password) return undefined;
  return { type: "basic", username: OPENCODE_BASIC_AUTH_USERNAME, password };
}

/** The server password OpenCode itself adopts, in the order it reads them. */
function ownServerPassword(env: V2ServerProcess["env"]): string | undefined {
  return env.OPENCODE_PASSWORD || env.OPENCODE_SERVER_PASSWORD || undefined;
}

type Probe =
  | { readonly kind: "answered"; readonly pid?: number }
  | { readonly kind: "unauthorized"; readonly status: number }
  | { readonly kind: "failed"; readonly detail: string };

/**
 * One authenticated call to the server's `/api/info`, which reports the pid of
 * the process serving it. That pid is what tells AFT's own server apart from
 * another OpenCode server on the same machine.
 */
async function probe(
  server: V2ServerProcess,
  endpoint: ServiceEndpoint,
  timeoutMs: number,
): Promise<Probe> {
  let response: Response;
  try {
    response = await server.fetch(`${endpoint.url.replace(/\/+$/, "")}/api/info`, {
      headers: headers(endpoint) ?? {},
      signal: AbortSignal.timeout(timeoutMs),
    });
  } catch (error) {
    return { kind: "failed", detail: detail(error) };
  }
  if (response.status === 401 || response.status === 403) {
    await response.body?.cancel().catch(() => {});
    return { kind: "unauthorized", status: response.status };
  }
  // A server that is still starting answers 503 with the same body, and the
  // pid in it is just as conclusive.
  if (!response.ok && response.status !== 503) {
    await response.body?.cancel().catch(() => {});
    return { kind: "failed", detail: `it answered HTTP ${response.status}` };
  }
  let body: unknown;
  try {
    body = await response.json();
  } catch {
    body = undefined;
  }
  const pid = (body as { pid?: unknown } | undefined)?.pid;
  if (typeof pid === "number" && Number.isInteger(pid)) return { kind: "answered", pid };
  return response.ok
    ? { kind: "answered" }
    : { kind: "failed", detail: `it answered HTTP ${response.status}` };
}

/** Use the server the user named in `opencode.server_url`, skipping discovery. */
async function configuredServer(
  settings: V2PromptServerSettings & { readonly server_url: string },
  server: V2ServerProcess,
  timeoutMs: number,
): Promise<V2PromptChannelResult> {
  const raw = settings.server_url.trim();
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return {
      reason: "server_url_invalid",
      unavailable: `"opencode.server_url" ${JSON.stringify(raw)} in your AFT config is not a URL; set it to the server's base URL, for example "http://127.0.0.1:4096"`,
    };
  }
  if ((url.protocol !== "http:" && url.protocol !== "https:") || url.username || url.password) {
    return {
      reason: "server_url_invalid",
      unavailable:
        `"opencode.server_url" ${JSON.stringify(raw)} must be an http(s) URL without credentials; ` +
        'put the password in an environment variable and name it in "opencode.server_password_env"',
    };
  }

  const variable = settings.server_password_env?.trim();
  let password: string | undefined;
  if (variable) {
    password = server.env[variable] || undefined;
    if (!password) {
      return {
        reason: "server_url_invalid",
        unavailable:
          `the environment variable ${variable} named by "opencode.server_password_env" is not ` +
          "set in the OpenCode process; set it to the server's password and restart OpenCode",
      };
    }
  } else {
    password = ownServerPassword(server.env);
  }

  const endpoint: ServiceEndpoint = {
    url: url.href.replace(/\/+$/, ""),
    auth: basicAuth(password),
  };
  const result = await probe(server, endpoint, timeoutMs);
  if (result.kind === "unauthorized") {
    return {
      reason: "server_url_unreachable",
      unavailable:
        `the server at "opencode.server_url" ${endpoint.url} refused AFT's credentials ` +
        `(HTTP ${result.status}); set "opencode.server_password_env" to the name of the ` +
        "environment variable holding that server's password",
    };
  }
  if (result.kind === "failed") {
    return {
      reason: "server_url_unreachable",
      unavailable: `the server at "opencode.server_url" ${endpoint.url} did not answer (${result.detail})`,
    };
  }
  if (result.pid !== undefined && result.pid !== server.pid) {
    // Allowed, because the user said so, but worth a line: prompts for this
    // process's sessions only show up if that server shares its sessions.
    warn(
      `[permissions] opencode.server_url ${endpoint.url} is served by process ${result.pid}, not ` +
        `by this OpenCode process (${server.pid}); permission prompts are raised there`,
    );
  }
  return { client: promptClient(endpoint) };
}

interface ListenAddress {
  readonly hostname: string;
  readonly port?: number;
  readonly stdio: boolean;
}

/** The value of `--name value` or `--name=value`, last occurrence wins. */
function flagValue(argv: readonly string[], name: string): string | undefined {
  let value: string | undefined;
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === `--${name}`) value = argv[index + 1];
    else if (arg?.startsWith(`--${name}=`)) value = arg.slice(name.length + 3);
  }
  return value;
}

/**
 * Where this process's own server listens, read from its command line.
 *
 * `opencode serve` takes `--hostname` (default 127.0.0.1) and `--port`. A
 * wildcard bind is reached on loopback. `--stdio` is how `opencode --standalone`
 * starts its private server: on a random port, with the password removed from
 * the environment, so it is reported rather than probed.
 */
export function ownListenAddress(argv: readonly string[]): ListenAddress {
  const rawHost = flagValue(argv, "hostname")?.trim();
  let hostname = rawHost ? rawHost.replace(/^\[(.*)\]$/, "$1") : "127.0.0.1";
  if (hostname === "0.0.0.0") hostname = "127.0.0.1";
  if (hostname === "::") hostname = "::1";
  const rawPort = flagValue(argv, "port")?.trim();
  const parsed = rawPort !== undefined && /^\d+$/.test(rawPort) ? Number(rawPort) : undefined;
  const port = parsed !== undefined && parsed <= 65535 ? parsed : undefined;
  return { hostname, port, stdio: argv.includes("--stdio") };
}

function serverURL(hostname: string, port: number): string {
  return `http://${hostname.includes(":") ? `[${hostname}]` : hostname}:${port}`;
}

/**
 * A service registration this process wrote for itself, if any.
 *
 * Only a registration whose recorded pid is this process's own is used: a TUI's
 * managed background service registers itself too, and a plain `opencode serve`
 * running beside it must not send its prompts there.
 */
async function ownRegistration(
  server: V2ServerProcess,
  timeoutMs: number,
  observed: string[],
): Promise<V2PromptChannelResult | undefined> {
  let registrations: readonly { readonly path: string; readonly text: string }[];
  try {
    registrations = await server.registrations();
  } catch (error) {
    observed.push(`reading OpenCode service registrations failed: ${detail(error)}`);
    return undefined;
  }
  for (const registration of registrations) {
    let info: { url?: unknown; pid?: unknown; password?: unknown };
    try {
      info = JSON.parse(registration.text) as typeof info;
    } catch {
      continue;
    }
    if (typeof info?.url !== "string" || typeof info.pid !== "number") continue;
    if (info.pid !== server.pid) {
      observed.push(
        `the service registered in ${registration.path} is process ${info.pid}, not this one (${server.pid}), so it was not used`,
      );
      continue;
    }
    const endpoint: ServiceEndpoint = {
      url: info.url.replace(/\/+$/, ""),
      auth: basicAuth(typeof info.password === "string" ? info.password : undefined),
    };
    const result = await probe(server, endpoint, timeoutMs);
    if (result.kind === "answered" && result.pid === server.pid) {
      return { client: promptClient(endpoint) };
    }
    observed.push(
      `this process's own registration ${registration.path} did not verify at ${endpoint.url}`,
    );
  }
  return undefined;
}

/** The address a client uses to reach a socket bound to `address`. */
function reachableHost(address: string): string {
  const host = address.replace(/^\[(.*)\]$/, "$1");
  if (host === "*" || host === "0.0.0.0" || host === "") return "127.0.0.1";
  if (/^[0:]+$/.test(host)) return "::1";
  return host;
}

/**
 * The endpoints to probe for this process's own server.
 *
 * Only ports the operating system says this very process listens on are
 * candidates, because each probe carries the server password: guessing ports
 * would hand it to whatever else listens there, such as another developer
 * server started in a worktree. An explicit `--port` must be one of those
 * ports; it is trusted on its own only when the operating system could not be
 * asked, because then the process's own command line is the best evidence.
 */
async function ownCandidates(
  server: V2ServerProcess,
  listen: ListenAddress,
  observed: string[],
): Promise<readonly string[] | undefined> {
  const declared = listen.port !== undefined && listen.port !== 0 ? listen.port : undefined;
  let sockets: readonly V2ListeningSocket[];
  try {
    sockets = await server.listeningSockets();
  } catch (error) {
    const failure = `looking up the ports this process (${server.pid}) listens on failed: ${detail(error)}`;
    if (declared === undefined) {
      observed.push(`${failure}, and its command line names no --port`);
      return undefined;
    }
    return [serverURL(listen.hostname, declared)];
  }
  const own =
    declared === undefined ? sockets : sockets.filter((socket) => socket.port === declared);
  if (own.length === 0) {
    const found = sockets.map((socket) => socket.port).join(", ") || "none";
    observed.push(
      declared === undefined
        ? `this process (${server.pid}) listens on no TCP port`
        : `--port ${declared} is not one of the ports this process (${server.pid}) listens on (${found})`,
    );
    return undefined;
  }
  return [...new Set(own.map((socket) => serverURL(reachableHost(socket.address), socket.port)))];
}

/** Find the server this process is, from its registration or its own listening ports. */
async function ownServer(
  server: V2ServerProcess,
  options: Required<V2DiscoveryOptions>,
): Promise<V2PromptChannelResult> {
  const observed: string[] = [];
  const registered = await ownRegistration(server, options.timeoutMs, observed);
  if (registered) return registered;

  const unaddressable = (): V2PromptChannelResult => ({
    reason: "own_server_unaddressable",
    unavailable: `AFT could not find the OpenCode server it runs in (${observed.join("; ")}); to fix it, ${HOW_TO_FIX}`,
  });

  const listen = ownListenAddress(server.argv);
  const password = ownServerPassword(server.env);
  if (listen.stdio) {
    observed.push(
      "this is a private server (opencode --standalone) whose password is not readable by plugins",
    );
    return unaddressable();
  }
  if (!password) {
    observed.push(
      "this server was started without OPENCODE_SERVER_PASSWORD, so it generated a password AFT cannot read",
    );
    return unaddressable();
  }

  const candidates = await ownCandidates(server, listen, observed);
  if (!candidates) return unaddressable();
  let refused = 0;
  let others = 0;
  for (const url of candidates) {
    const endpoint: ServiceEndpoint = { url, auth: basicAuth(password) };
    const result = await probe(server, endpoint, options.timeoutMs);
    if (result.kind === "answered" && result.pid === server.pid) {
      return { client: promptClient(endpoint) };
    }
    if (result.kind === "unauthorized") refused += 1;
    if (result.kind === "answered") others += 1;
  }
  observed.push(
    `none of ${candidates.join(", ")} answered as this process (${server.pid})` +
      (refused > 0 ? `; ${refused} refused the password from OPENCODE_SERVER_PASSWORD` : "") +
      (others > 0 ? `; ${others} belonged to another process` : ""),
  );
  return unaddressable();
}

/**
 * Pick the OpenCode server a permission prompt is raised on.
 *
 * The user's `opencode.server_url` wins and skips discovery. Without it, AFT
 * looks for the server it is itself running in: first a service registration
 * whose pid is this process, then the TCP ports this process listens on, with
 * the password it was started with. Every candidate is verified with one
 * authenticated health call whose answer must name this process, so a prompt
 * never goes to some other OpenCode server that happens to be running.
 */
export async function discoverPromptServer(
  settings: V2PromptServerSettings | undefined,
  server: V2ServerProcess = currentServerProcess(),
  options: V2DiscoveryOptions = {},
): Promise<V2PromptChannelResult> {
  const resolved: Required<V2DiscoveryOptions> = {
    timeoutMs: options.timeoutMs ?? DEFAULT_PROBE_TIMEOUT_MS,
  };
  const serverUrl = settings?.server_url;
  if (serverUrl !== undefined && serverUrl.trim() !== "") {
    return await configuredServer(
      { ...settings, server_url: serverUrl },
      server,
      resolved.timeoutMs,
    );
  }
  return await ownServer(server, resolved);
}

/**
 * Where OpenCode 2 keeps its service registrations: `service.json` for the
 * stable channels and `service-<channel>.json` for the others, under
 * `$XDG_STATE_HOME/opencode` (default `~/.local/state/opencode`).
 */
function registrationDirectory(env: V2ServerProcess["env"]): string {
  return join(env.XDG_STATE_HOME || join(homedir(), ".local", "state"), "opencode");
}

/** This process, as discovery sees it. */
export function currentServerProcess(): V2ServerProcess {
  const env = globalThis.process.env;
  return {
    pid: globalThis.process.pid,
    argv: globalThis.process.argv,
    env,
    async registrations() {
      const directory = registrationDirectory(env);
      const names = await readdir(directory).catch(() => [] as string[]);
      const files = names.filter((name) => /^service(-[^/\\]+)?\.json$/.test(name)).sort();
      const read = await Promise.all(
        files.map(async (name) => {
          const path = join(directory, name);
          const text = await readFile(path, "utf8").catch(() => undefined);
          return text === undefined ? undefined : { path, text };
        }),
      );
      return read.filter((entry) => entry !== undefined);
    },
    listeningSockets: () => listeningSocketsOf(globalThis.process.pid),
    fetch: (url, init) => fetch(url, init),
  };
}

/** Run a short system command, bounded, with no console window on Windows. */
function runCommand(file: string, args: readonly string[]): Promise<string> {
  return new Promise((resolve, reject) => {
    execFile(
      file,
      [...args],
      { timeout: LISTEN_LOOKUP_TIMEOUT_MS, maxBuffer: 8 * 1024 * 1024, encoding: "utf8" },
      (error, stdout) => {
        // lsof exits 1 when it matched nothing; that is an empty answer, not a failure.
        const code = (error as { code?: unknown } | null)?.code;
        if (error && !(code === 1 && file === "lsof")) reject(error);
        else resolve(String(stdout ?? ""));
      },
    );
  });
}

/** Split `host:port`, `[v6]:port` or `*:port` into its parts. */
function splitHostPort(value: string): V2ListeningSocket | undefined {
  const separator = value.lastIndexOf(":");
  if (separator < 0) return undefined;
  const port = Number(value.slice(separator + 1));
  if (!Number.isInteger(port) || port <= 0 || port > 65535) return undefined;
  return { address: value.slice(0, separator).replace(/^\[(.*)\]$/, "$1"), port };
}

/** Parse `lsof -F n` output: one `n<address>:<port>` line per socket. */
export function parseLsofListening(output: string): V2ListeningSocket[] {
  return output
    .split(/\r?\n/)
    .filter((line) => line.startsWith("n"))
    .map((line) => splitHostPort(line.slice(1)))
    .filter((socket) => socket !== undefined);
}

/**
 * Parse `netstat -ano` output for one pid. A listening row is recognized by
 * its zero foreign port rather than by the state column, which Windows
 * translates on non-English systems.
 */
export function parseNetstatListening(output: string, pid: number): V2ListeningSocket[] {
  const sockets: V2ListeningSocket[] = [];
  for (const line of output.split(/\r?\n/)) {
    const columns = line.trim().split(/\s+/);
    if (columns.length < 5 || !columns[0]?.toUpperCase().startsWith("TCP")) continue;
    if (Number(columns[columns.length - 1]) !== pid) continue;
    if (!/:0$/.test(columns[2] ?? "")) continue;
    const socket = splitHostPort(columns[1] ?? "");
    if (socket) sockets.push(socket);
  }
  return sockets;
}

/**
 * Decode an address from `/proc/net/tcp{,6}`, where each 8-digit hex word
 * stores its bytes in little-endian order (so 127.0.0.1 reads `0100007F`).
 */
function procAddress(hex: string): string {
  const bytes: number[] = [];
  for (let word = 0; word < hex.length; word += 8) {
    const chunk = hex.slice(word, word + 8);
    for (let index = 6; index >= 0; index -= 2) {
      bytes.push(Number.parseInt(chunk.slice(index, index + 2), 16));
    }
  }
  if (bytes.length === 4) return bytes.join(".");
  const groups: string[] = [];
  for (let index = 0; index < bytes.length; index += 2) {
    groups.push((((bytes[index] ?? 0) << 8) | (bytes[index + 1] ?? 0)).toString(16));
  }
  return groups.join(":");
}

/**
 * Parse `/proc/net/tcp` or `/proc/net/tcp6` rows that are listening (state
 * `0A`) on one of the given socket inodes.
 */
export function parseProcNetListening(
  table: string,
  inodes: ReadonlySet<string>,
): V2ListeningSocket[] {
  const sockets: V2ListeningSocket[] = [];
  for (const line of table.split(/\r?\n/).slice(1)) {
    const columns = line.trim().split(/\s+/);
    if (columns[3] !== "0A" || !inodes.has(columns[9] ?? "")) continue;
    const [address, port] = (columns[1] ?? "").split(":");
    if (!address || !port) continue;
    sockets.push({ address: procAddress(address), port: Number.parseInt(port, 16) });
  }
  return sockets;
}

/** Linux without lsof: match this process's socket descriptors to the kernel's tables. */
async function procListening(pid: number): Promise<V2ListeningSocket[]> {
  const descriptors = await readdir(`/proc/${pid}/fd`);
  const inodes = new Set<string>();
  await Promise.all(
    descriptors.map(async (fd) => {
      const target = await readlink(`/proc/${pid}/fd/${fd}`).catch(() => "");
      const inode = /^socket:\[(\d+)\]$/.exec(target)?.[1];
      if (inode) inodes.add(inode);
    }),
  );
  const tables = await Promise.all(
    ["/proc/net/tcp", "/proc/net/tcp6"].map((path) => readFile(path, "utf8").catch(() => "")),
  );
  return tables.flatMap((table) => parseProcNetListening(table, inodes));
}

/** Ask the operating system which TCP ports `pid` listens on. */
export async function listeningSocketsOf(pid: number): Promise<V2ListeningSocket[]> {
  if (globalThis.process.platform === "win32") {
    return parseNetstatListening(await runCommand("netstat", ["-ano", "-p", "TCP"]), pid).concat(
      parseNetstatListening(await runCommand("netstat", ["-ano", "-p", "TCPv6"]), pid),
    );
  }
  try {
    return parseLsofListening(
      await runCommand("lsof", ["-nP", "-a", "-p", String(pid), "-iTCP", "-sTCP:LISTEN", "-Fn"]),
    );
  } catch (error) {
    const missing = (error as { code?: unknown }).code === "ENOENT";
    if (!missing || globalThis.process.platform !== "linux") throw error;
    return await procListening(pid);
  }
}

/**
 * One prompt channel for one OpenCode Location.
 *
 * The discovered client is kept so a session that answers many prompts pays for
 * discovery once, and it is held in this closure rather than in module state so
 * it is dropped with the Location's consumers when that scope is torn down.
 * Only a successful discovery is remembered: a host whose server was not up
 * yet must be able to raise the next prompt. The first failure is logged, once,
 * so the plugin log names the reason even when no prompt refusal is read.
 */
export function createV2PromptChannel(
  discovery: () => Promise<V2PromptChannelResult> = () => discoverPromptServer(undefined),
): V2PromptChannel {
  let cached: V2PermissionClient | undefined;
  let failureLogged = false;
  return {
    async client() {
      if (cached) return { client: cached };
      const result = await discovery();
      if (result.client) {
        cached = result.client;
        log("[permissions] OpenCode server for permission prompts found");
      } else if (!failureLogged) {
        failureLogged = true;
        warn(
          `[permissions] permission prompts are unavailable${result.reason ? ` (${result.reason})` : ""}: ${result.unavailable}`,
        );
      }
      return result;
    },
  };
}
