import { afterEach, describe, expect, test } from "bun:test";
import { Effect } from "effect";
import {
  PermissionPromptUnavailableError,
  requestPermission,
  type V2PermissionHostContext,
} from "../../src/permissions/v2.js";
import {
  createV2PromptChannel,
  currentServerProcess,
  discoverPromptServer,
  ownListenAddress,
  parseLsofListening,
  parseNetstatListening,
  parseProcNetListening,
  type V2ListeningSocket,
  type V2ServerProcess,
} from "../../src/permissions/v2-service.js";

/**
 * Discovery of the OpenCode 2 server a permission prompt is raised on.
 *
 * Every server here is a real HTTP listener that behaves like OpenCode 2's
 * `/api/info`: it insists on Basic auth with the fixed username `opencode` and
 * reports the pid of the process serving it. Each one stands in for a
 * different OpenCode process by reporting its own pid, which is exactly how
 * discovery tells its own server apart from another one on the machine.
 */

const OWN_PID = 424242;
const TUI_PID = 515151;

interface FakeServer {
  readonly url: string;
  readonly port: number;
  /** Every request this server received, as `METHOD path authorization`. */
  readonly requests: string[];
  stop(): void;
}

const running: FakeServer[] = [];

afterEach(() => {
  for (const server of running.splice(0)) server.stop();
});

function basic(password: string): string {
  return `Basic ${Buffer.from(`opencode:${password}`).toString("base64")}`;
}

function fakeOpenCodeServer(pid: number, password: string, port = 0): FakeServer {
  const requests: string[] = [];
  const server = Bun.serve({
    port,
    hostname: "127.0.0.1",
    fetch(request) {
      const path = new URL(request.url).pathname;
      const authorization = request.headers.get("authorization") ?? "";
      requests.push(`${request.method} ${path} ${authorization}`);
      if (authorization !== basic(password)) return new Response("unauthorized", { status: 401 });
      if (path === "/api/info") return Response.json({ version: "2.0.21", pid, urls: [] });
      return Response.json({ id: "per-1", effect: "allow" });
    },
  });
  const fake: FakeServer = {
    url: `http://127.0.0.1:${server.port}`,
    port: server.port as number,
    requests,
    stop: () => server.stop(true),
  };
  running.push(fake);
  return fake;
}

/**
 * Two listeners on consecutive ports: someone else's server, and this
 * process's server on the port after it, where `opencode serve` lands when
 * the first one is taken.
 */
function consecutiveServers(
  first: { pid: number; password: string },
  second: { pid: number; password: string },
): [FakeServer, FakeServer] {
  for (let attempt = 0; attempt < 20; attempt += 1) {
    const a = fakeOpenCodeServer(first.pid, first.password);
    try {
      const b = fakeOpenCodeServer(second.pid, second.password, a.port + 1);
      return [a, b];
    } catch {
      a.stop();
      running.splice(running.indexOf(a), 1);
    }
  }
  throw new Error("could not bind two consecutive loopback ports");
}

function serverProcess(options: {
  argv?: string[];
  env?: Record<string, string | undefined>;
  registrations?: { path: string; text: string }[];
  /** What the operating system reports this process listens on, or why it could not say. */
  listening?: V2ListeningSocket[] | Error;
}): V2ServerProcess & { readonly lookups: () => number } {
  let lookups = 0;
  return {
    pid: OWN_PID,
    argv: ["/usr/local/bin/opencode", "/$bunfs/root/opencode", ...(options.argv ?? ["serve"])],
    env: options.env ?? {},
    registrations: async () => options.registrations ?? [],
    listeningSockets: async () => {
      lookups += 1;
      if (options.listening instanceof Error) throw options.listening;
      return options.listening ?? [];
    },
    fetch: (url, init) => fetch(url, init),
    lookups: () => lookups,
  };
}

/** The socket an `opencode serve --hostname 0.0.0.0` reports for this server. */
const listensOn = (server: FakeServer, address = "*"): V2ListeningSocket => ({
  address,
  port: server.port,
});

function registration(server: FakeServer, pid: number, password: string) {
  return {
    path: "/state/opencode/service.json",
    text: JSON.stringify({ id: "svc", version: "2.0.21", url: server.url, pid, password }),
  };
}

const probes = (server: FakeServer) =>
  server.requests.filter((entry) => entry.startsWith("GET /api/info"));

const EDIT_REQUEST = {
  permission: "edit",
  patterns: ["file.ts"],
  always: ["file.ts"],
  metadata: {},
};
const CONTEXT = {
  sessionID: "session-v2",
  messageID: "message-v2",
  id: "call-v2",
  agent: "agent-v2",
  progress: () => Effect.succeed(undefined),
};
const askEverything: V2PermissionHostContext = {
  agent: {
    get: () =>
      Effect.succeed({ data: { permissions: [{ action: "*", resource: "*", effect: "ask" }] } }),
  },
  session: { get: () => Effect.succeed({}) },
};

describe("OpenCode V2 prompt server: the opencode.server_url setting", () => {
  test("wins over discovery and is used with the password its variable names", async () => {
    const configured = fakeOpenCodeServer(OWN_PID, "configured-secret");
    const own = fakeOpenCodeServer(OWN_PID, "own-secret");
    const process = serverProcess({
      argv: ["serve", "--port", String(own.port)],
      env: { MY_OPENCODE_PASSWORD: "configured-secret", OPENCODE_SERVER_PASSWORD: "own-secret" },
      registrations: [registration(own, OWN_PID, "own-secret")],
    });

    const result = await discoverPromptServer(
      { server_url: `${configured.url}/`, server_password_env: "MY_OPENCODE_PASSWORD" },
      process,
    );

    expect(result.unavailable).toBeUndefined();
    expect(probes(configured)).toEqual([`GET /api/info ${basic("configured-secret")}`]);
    // Discovery never ran: the server this process would have found was not asked.
    expect(own.requests).toEqual([]);

    // The client really targets the configured server.
    await result.client?.permission
      .create({
        sessionID: "s",
        action: "edit",
        resources: ["f"],
        save: [],
        metadata: {},
        source: { type: "tool", messageID: "m", id: "c" },
      })
      .catch(() => undefined);
    expect(configured.requests.some((entry) => !entry.startsWith("GET /api/info"))).toBe(true);
    expect(own.requests).toEqual([]);
  });

  test("falls back to OpenCode's own password variables when no variable is named", async () => {
    const configured = fakeOpenCodeServer(OWN_PID, "env-secret");
    const result = await discoverPromptServer(
      { server_url: configured.url },
      serverProcess({ env: { OPENCODE_PASSWORD: "env-secret" } }),
    );
    expect(result.unavailable).toBeUndefined();
    expect(probes(configured)).toEqual([`GET /api/info ${basic("env-secret")}`]);
  });

  test("an unset password variable is a named, fixable refusal", async () => {
    const configured = fakeOpenCodeServer(OWN_PID, "secret");
    const result = await discoverPromptServer(
      { server_url: configured.url, server_password_env: "MISSING_PASSWORD" },
      serverProcess({}),
    );
    expect(result.reason).toBe("server_url_invalid");
    expect(result.unavailable).toContain("MISSING_PASSWORD");
    expect(configured.requests).toEqual([]);
  });

  test("a server that refuses the credentials is reported, not used", async () => {
    const configured = fakeOpenCodeServer(OWN_PID, "right");
    const result = await discoverPromptServer(
      { server_url: configured.url, server_password_env: "PW" },
      serverProcess({ env: { PW: "wrong" } }),
    );
    expect(result.client).toBeUndefined();
    expect(result.reason).toBe("server_url_unreachable");
    expect(result.unavailable).toContain("refused AFT's credentials (HTTP 401)");
  });

  test("a URL with embedded credentials is rejected before any request", async () => {
    const result = await discoverPromptServer(
      { server_url: "http://opencode:secret@127.0.0.1:4096" },
      serverProcess({}),
    );
    expect(result.reason).toBe("server_url_invalid");
    expect(result.unavailable).toContain("opencode.server_password_env");
  });
});

describe("OpenCode V2 prompt server: discovering the server AFT runs in", () => {
  test("probes the port this process listens on, with its OPENCODE_SERVER_PASSWORD", async () => {
    const own = fakeOpenCodeServer(OWN_PID, "own-secret");
    const process = serverProcess({
      argv: ["serve", `--port=${own.port}`, "--hostname", "0.0.0.0"],
      env: { OPENCODE_SERVER_PASSWORD: "own-secret" },
      listening: [listensOn(own, "0.0.0.0")],
    });
    const result = await discoverPromptServer(undefined, process);
    expect(result.unavailable).toBeUndefined();
    expect(process.lookups()).toBe(1);
    expect(probes(own)).toEqual([`GET /api/info ${basic("own-secret")}`]);
  });

  test("without --port finds the port from this process's own listening sockets", async () => {
    const own = fakeOpenCodeServer(OWN_PID, "own-secret");
    const result = await discoverPromptServer(
      undefined,
      serverProcess({
        env: { OPENCODE_SERVER_PASSWORD: "own-secret" },
        listening: [listensOn(own, "127.0.0.1")],
      }),
    );
    expect(result.unavailable).toBeUndefined();
    expect(probes(own)).toEqual([`GET /api/info ${basic("own-secret")}`]);
  });

  test("a server on the next port that this process does not own is never sent the password", async () => {
    // `opencode serve` without --port starts at 4096 and moves up, so another
    // server often sits on the port a walk from 4096 would reach first. Only
    // ports this process listens on may receive the password.
    const [next, own] = consecutiveServers(
      { pid: TUI_PID, password: "shared-secret" },
      { pid: OWN_PID, password: "shared-secret" },
    );
    const result = await discoverPromptServer(
      undefined,
      serverProcess({
        env: { OPENCODE_SERVER_PASSWORD: "shared-secret" },
        listening: [listensOn(own)],
      }),
    );
    expect(result.unavailable).toBeUndefined();
    expect(next.requests).toEqual([]);
    await result.client?.permission
      .create({
        sessionID: "s",
        action: "edit",
        resources: ["f"],
        save: [],
        metadata: {},
        source: { type: "tool", messageID: "m", id: "c" },
      })
      .catch(() => undefined);
    expect(own.requests.some((entry) => !entry.startsWith("GET /api/info"))).toBe(true);
    expect(next.requests).toEqual([]);
  });

  test("a failed port lookup refuses without probing anything", async () => {
    const own = fakeOpenCodeServer(OWN_PID, "own-secret");
    const result = await discoverPromptServer(
      undefined,
      serverProcess({
        env: { OPENCODE_SERVER_PASSWORD: "own-secret" },
        listening: new Error("spawn lsof ENOENT"),
      }),
    );
    expect(result.client).toBeUndefined();
    expect(result.reason).toBe("own_server_unaddressable");
    expect(result.unavailable).toContain("spawn lsof ENOENT");
    expect(result.unavailable).toContain('"opencode.server_url"');
    expect(own.requests).toEqual([]);
  });

  test("a failed port lookup still trusts the --port this process was started with", async () => {
    const own = fakeOpenCodeServer(OWN_PID, "own-secret");
    const result = await discoverPromptServer(
      undefined,
      serverProcess({
        argv: ["serve", "--port", String(own.port)],
        env: { OPENCODE_SERVER_PASSWORD: "own-secret" },
        listening: new Error("lsof timed out"),
      }),
    );
    expect(result.unavailable).toBeUndefined();
    expect(probes(own)).toEqual([`GET /api/info ${basic("own-secret")}`]);
  });

  test("a --port this process does not listen on is refused without a probe", async () => {
    const elsewhere = fakeOpenCodeServer(TUI_PID, "own-secret");
    const own = fakeOpenCodeServer(OWN_PID, "own-secret");
    const result = await discoverPromptServer(
      undefined,
      serverProcess({
        argv: ["serve", "--port", String(elsewhere.port)],
        env: { OPENCODE_SERVER_PASSWORD: "own-secret" },
        listening: [listensOn(own)],
      }),
    );
    expect(result.reason).toBe("own_server_unaddressable");
    expect(result.unavailable).toContain(`--port ${elsewhere.port} is not one of the ports`);
    expect(elsewhere.requests).toEqual([]);
    expect(own.requests).toEqual([]);
  });

  test("finds a server in this very process through the real port lookup", async () => {
    // The test process plays the OpenCode server: it listens, answers with its
    // own pid, and the platform's lookup has to report that port.
    const own = fakeOpenCodeServer(globalThis.process.pid, "real-secret");
    const real = currentServerProcess();
    const sockets = await real.listeningSockets();
    expect(sockets.map((socket) => socket.port)).toContain(own.port);

    const result = await discoverPromptServer(undefined, {
      ...real,
      argv: ["opencode", "serve"],
      env: { OPENCODE_SERVER_PASSWORD: "real-secret" },
      registrations: async () => [],
    });
    expect(result.unavailable).toBeUndefined();
    expect(probes(own)).toEqual([`GET /api/info ${basic("real-secret")}`]);
  });

  test("reads lsof, netstat and /proc listening tables", () => {
    expect(
      parseLsofListening("p42\nf12\nn*:4096\nf13\nn127.0.0.1:4097\nf14\nn[::1]:4098\n"),
    ).toEqual([
      { address: "*", port: 4096 },
      { address: "127.0.0.1", port: 4097 },
      { address: "::1", port: 4098 },
    ]);
    const netstat = [
      "  Proto  Local Address          Foreign Address        State           PID",
      "  TCP    0.0.0.0:4096           0.0.0.0:0              LISTENING       42",
      "  TCP    127.0.0.1:5000         0.0.0.0:0              ABHÖREN         42",
      "  TCP    127.0.0.1:4096         127.0.0.1:50000        ESTABLISHED     42",
      "  TCP    0.0.0.0:7000           0.0.0.0:0              LISTENING       7",
      "  TCP    [::]:4100              [::]:0                 LISTENING       42",
    ].join("\r\n");
    expect(parseNetstatListening(netstat, 42)).toEqual([
      { address: "0.0.0.0", port: 4096 },
      { address: "127.0.0.1", port: 5000 },
      { address: "::", port: 4100 },
    ]);
    const tcp = [
      "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode",
      "   0: 0100007F:1000 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 111 1",
      "   1: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 222 1",
      "   2: 0100007F:1001 0100007F:C350 01 00000000:00000000 00:00000000 00000000  1000        0 111 1",
    ].join("\n");
    expect(parseProcNetListening(tcp, new Set(["111"]))).toEqual([
      { address: "127.0.0.1", port: 4096 },
    ]);
    const tcp6 = [
      "  sl  local_address                         remote_address                        st",
      "   0: 00000000000000000000000001000000:1000 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 333 1",
    ].join("\n");
    expect(parseProcNetListening(tcp6, new Set(["333"]))).toEqual([
      { address: "0:0:0:0:0:0:0:1", port: 4096 },
    ]);
  });

  test("ignores a service.json written by another process (a TUI's background service)", async () => {
    const tuiService = fakeOpenCodeServer(TUI_PID, "tui-secret");
    const own = fakeOpenCodeServer(OWN_PID, "own-secret");
    const result = await discoverPromptServer(
      undefined,
      serverProcess({
        argv: ["serve", "--port", String(own.port)],
        env: { OPENCODE_SERVER_PASSWORD: "own-secret" },
        registrations: [registration(tuiService, TUI_PID, "tui-secret")],
        listening: [listensOn(own)],
      }),
    );
    expect(result.unavailable).toBeUndefined();
    // The TUI's service was never contacted, so its password never left the file.
    expect(tuiService.requests).toEqual([]);
    expect(probes(own)).toEqual([`GET /api/info ${basic("own-secret")}`]);
  });

  test("a foreign service.json alone is never used, and the refusal says why", async () => {
    const tuiService = fakeOpenCodeServer(TUI_PID, "tui-secret");
    const result = await discoverPromptServer(
      undefined,
      serverProcess({ registrations: [registration(tuiService, TUI_PID, "tui-secret")] }),
    );
    expect(result.client).toBeUndefined();
    expect(result.reason).toBe("own_server_unaddressable");
    expect(result.unavailable).toContain(`is process ${TUI_PID}, not this one (${OWN_PID})`);
    expect(tuiService.requests).toEqual([]);
  });

  test("uses a service.json this process registered for itself (opencode serve --service)", async () => {
    const own = fakeOpenCodeServer(OWN_PID, "service-secret");
    const result = await discoverPromptServer(
      undefined,
      serverProcess({
        argv: ["serve", "--service"],
        registrations: [registration(own, OWN_PID, "service-secret")],
      }),
    );
    expect(result.unavailable).toBeUndefined();
    expect(probes(own)).toEqual([`GET /api/info ${basic("service-secret")}`]);
  });

  test("a registration naming this pid whose server answers as another process is not trusted", async () => {
    const impostor = fakeOpenCodeServer(TUI_PID, "secret");
    const result = await discoverPromptServer(
      undefined,
      serverProcess({ registrations: [registration(impostor, OWN_PID, "secret")] }),
    );
    expect(result.client).toBeUndefined();
    expect(result.reason).toBe("own_server_unaddressable");
  });

  test("reads OpenCode's serve flags in both spellings", () => {
    expect(ownListenAddress(["opencode", "serve"])).toEqual({
      hostname: "127.0.0.1",
      port: undefined,
      stdio: false,
    });
    expect(ownListenAddress(["serve", "--hostname=::", "--port", "4100"])).toEqual({
      hostname: "::1",
      port: 4100,
      stdio: false,
    });
    expect(ownListenAddress(["serve", "--stdio", "--port", "0"]).stdio).toBe(true);
  });
});

describe("OpenCode V2 prompt server: nothing addressable", () => {
  test("a standalone private server is refused with a named reason and the fix", async () => {
    const result = await discoverPromptServer(
      undefined,
      serverProcess({ argv: ["serve", "--stdio", "--port", "0"] }),
    );
    expect(result.reason).toBe("own_server_unaddressable");
    expect(result.unavailable).toContain("opencode --standalone");
    expect(result.unavailable).toContain('"opencode.server_url"');
    expect(result.unavailable).toContain("OPENCODE_SERVER_PASSWORD");
  });

  test("a serve without a readable password is refused without probing anything", async () => {
    const own = fakeOpenCodeServer(OWN_PID, "generated");
    const result = await discoverPromptServer(
      undefined,
      serverProcess({ argv: ["serve", "--port", String(own.port)] }),
    );
    expect(result.reason).toBe("own_server_unaddressable");
    expect(result.unavailable).toContain("started without OPENCODE_SERVER_PASSWORD");
    expect(own.requests).toEqual([]);
  });

  test("the prompt refusal carries the reason and still fails closed", async () => {
    const channel = createV2PromptChannel(() =>
      discoverPromptServer(undefined, serverProcess({ argv: ["serve", "--stdio"] })),
    );
    const refusal = await requestPermission(askEverything, EDIT_REQUEST, CONTEXT, channel).catch(
      (error: unknown) => error as Error,
    );
    expect(refusal).toBeInstanceOf(PermissionPromptUnavailableError);
    expect(refusal.message).toContain("could not find the OpenCode server it runs in");
    expect(refusal.message).toContain('"opencode.server_url"');
    expect(refusal.message).not.toContain("Start OpenCode's background service");
  });
});
