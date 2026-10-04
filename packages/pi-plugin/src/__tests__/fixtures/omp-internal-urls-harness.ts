import assert from "node:assert/strict";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

// This file runs only in a child with throwaway HOME/XDG directories. Loading
// OMP in the unit-test process would install its process-global Pi import shim.
const hostRoot = process.env.AFT_OMP_HOST;
assert(hostRoot, "AFT_OMP_HOST must contain an installed OMP host");
const entry = Bun.resolveSync("@oh-my-pi/pi-coding-agent", hostRoot);
const packageInfo = await Bun.file(resolve(dirname(entry), "../package.json")).json();
assert.match(packageInfo.version, /^18\.6\./, "The headless check requires an OMP 18.6.x host");
const host = await import(entry);
const root = process.env.HOME!;
const cwd = join(root, "project");
const skillDir = join(root, "skills", "aft-handback");
mkdirSync(cwd, { recursive: true });
mkdirSync(skillDir, { recursive: true });
const skillFile = join(skillDir, "SKILL.md");
writeFileSync(
  skillFile,
  "---\nname: aft-handback\ndescription: Internal URL hand-back test skill\n---\nOMP skill payload\n",
);
const settings = host.Settings.isolated({
  skillful: true,
  launch: { enabled: false },
  async: { enabled: true },
});
const authStorage = await host.AuthStorage.create(join(root, "auth.db"));
const modelRegistry = new host.ModelRegistry(authStorage, join(root, "models.yml"), {
  settings,
  fetch: () => Promise.reject(new Error("Network is disabled in the internal URL headless check")),
});
const { session, extensionsResult } = await host.createAgentSession({
  cwd,
  agentDir: join(root, "agent"),
  settings,
  authStorage,
  modelRegistry,
  model: modelRegistry.getAll()[0],
  additionalExtensionPaths: [resolve(import.meta.dir, "omp-internal-urls-extension.ts")],
  disableExtensionDiscovery: true,
  enableMCP: false,
  enableLsp: false,
  skipPythonPreflight: true,
  cacheWarming: false,
  contextFiles: [],
  rules: [],
  skills: [
    {
      name: "aft-handback",
      description: "Internal URL hand-back test skill",
      filePath: skillFile,
      baseDir: skillDir,
      source: "test",
    },
  ],
  toolNames: ["read", "write", "edit", "grep", "bash"],
  autoApprove: true,
});

function text(result: any): string {
  return result.content
    .filter((block: any) => block.type === "text")
    .map((block: any) => block.text)
    .join("\n");
}

try {
  assert.equal(extensionsResult.errors.length, 0, JSON.stringify(extensionsResult.errors));
  const aftTools = extensionsResult.extensions.flatMap((extension: any) => [
    ...extension.tools.values(),
  ]);
  const bash = aftTools.find((tool: any) => (tool.definition ?? tool).name === "bash");
  assert(bash, "AFT bash override was not installed");
  const call = async (name: string, params: Record<string, unknown>) => {
    const tool = session.getToolByName(name);
    assert(tool, `${name} is not active`);
    const prepared = tool.prepareArguments ? tool.prepareArguments(params) : params;
    return await tool.execute(`headless-${name}`, prepared, undefined, undefined);
  };
  const agentRead = text(await call("read", { path: "agent://Main" }));
  assert.match(agentRead, /Main/);
  const skillRead = text(await call("read", { path: "skill://aft-handback" }));
  assert.match(skillRead, /OMP skill payload/);
  const prompt = session.systemPrompt.join("\n");
  assert.match(prompt, /<skills>\s*- aft-handback: Internal URL hand-back test skill\s*<\/skills>/);
  const manager = session.asyncJobManager;
  assert(manager, "OMP async manager was not created");
  let cancelled = false;
  const jobId = manager.register(
    "bash",
    "AFT hand-back cancellation",
    async ({ signal }: any) => {
      await new Promise<void>((resolve) =>
        signal.addEventListener(
          "abort",
          () => {
            cancelled = true;
            resolve();
          },
          { once: true },
        ),
      );
      return "cancelled";
    },
    { ownerId: "Main" },
  );
  const killed = text(await call("write", { path: `proc://${jobId}/kill` }));
  assert(cancelled, "native proc kill did not abort its OMP-owned job");
  assert.equal(manager.getJob(jobId).status, "cancelled");
  const routerModule = await import(
    Bun.resolveSync("@oh-my-pi/pi-coding-agent/internal-urls", dirname(entry))
  );
  assert.equal(routerModule.InternalUrlRouter.instance().canHandle("mcp://x"), true);
  console.log(
    JSON.stringify({
      version: `OMP ${packageInfo.version}`,
      checks: 5,
      agentRead,
      skillRead,
      killed,
      skillPrompt: true,
      registeredMcp: true,
    }),
  );
} finally {
  await session.dispose();
}
