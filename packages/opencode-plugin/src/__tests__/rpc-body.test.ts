import { expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { AftRpcServer } from "../shared/rpc-server.js";

for (const runtime of ["Bun", "Node"] as const) {
  test(`${runtime} RPC rejects non-object bodies and stays alive`, async () => {
    const root = mkdtempSync(join(tmpdir(), "aft-rpc-body-"));
    const server = new AftRpcServer(root, root);
    server.handle("echo", async () => ({ ok: true }));
    try {
      // Exercise both real HTTP listeners without changing the test runner's globals.
      const port =
        runtime === "Bun"
          ? await server.start()
          : await (server as unknown as { startNode(): Promise<number> }).startNode();
      for (const body of ["null", "1", "[]", '"x"']) {
        const response = await fetch(`http://127.0.0.1:${port}/rpc/echo`, { method: "POST", body });
        expect(response.status).toBe(400);
        expect((await fetch(`http://127.0.0.1:${port}/health`)).status).toBe(200);
      }
    } finally {
      server.stop();
      rmSync(root, { recursive: true, force: true });
    }
  });
}

test("Node process survives invalid RPC JSON envelopes", async () => {
  const root = mkdtempSync(join(tmpdir(), "aft-rpc-node-"));
  try {
    const bundle = join(root, "server.mjs");
    const built = await Bun.build({
      entrypoints: [join(import.meta.dir, "../shared/rpc-server.ts")],
      target: "node",
    });
    expect(built.success).toBe(true);
    await Bun.write(bundle, built.outputs[0]!);
    const child = Bun.spawn(
      [
        "node",
        "--input-type=module",
        "-e",
        `
      import { AftRpcServer } from ${JSON.stringify(bundle)};
      const server = new AftRpcServer(${JSON.stringify(root)}, ${JSON.stringify(root)});
      server.handle("echo", async () => ({ok:true}));
      const port = await server.start();
      try {
        for (const body of ["null", "1", "[]", '"x"']) {
          const res = await fetch('http://127.0.0.1:'+port+'/rpc/echo', {method:'POST',body,signal:AbortSignal.timeout(2000)});
          if (res.status !== 400) throw new Error(body+': '+res.status);
          const health = await fetch('http://127.0.0.1:'+port+'/health');
          if (health.status !== 200) throw new Error('server died');
        }
      } finally { server.stop(); }
    `,
      ],
      { stdout: "pipe", stderr: "pipe" },
    );
    const stderr = await new Response(child.stderr).text();
    expect({ code: await child.exited, stderr }).toEqual({ code: 0, stderr: "" });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
