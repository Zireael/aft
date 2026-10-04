import type {
  AgentToolResult,
  ExtensionContext,
  ToolDefinition,
} from "@earendil-works/pi-coding-agent";
import { Text } from "@earendil-works/pi-tui";
import { type Static, type TSchema, Type } from "typebox";
import { Value } from "typebox/value";
import { detectPiHarness } from "./harness.js";
import {
  asRecord,
  asString,
  collapsibleResult,
  type RenderContextLike,
  type RenderResultOptionsLike,
} from "./tools/render-helpers.js";

/** The host-owned router API used by the overrides; no URL handlers live in AFT. */
export interface OmpInternalUrlRouter {
  canHandle(input: string): boolean;
  canResolve(input: string): boolean;
  writeTarget(input: string): { spec: { write?: unknown } } | undefined;
}

export async function loadOmpInternalUrlRouter(
  api: unknown,
): Promise<OmpInternalUrlRouter | undefined> {
  if (detectPiHarness(api) !== "omp") return undefined;
  try {
    // Keep the literal import: OMP's extension loader resolves host subpaths,
    // including in standalone binaries where there is no host node_modules.
    const { InternalUrlRouter } = await import("@oh-my-pi/pi-coding-agent/internal-urls");
    const router = InternalUrlRouter.instance();
    if (
      typeof router.canHandle === "function" &&
      typeof router.canResolve === "function" &&
      typeof router.writeTarget === "function"
    )
      return router;
  } catch {
    // Upstream Pi and older compatible hosts need no optional OMP dependency.
  }
  return undefined;
}

type OmpContext = ExtensionContext & {
  invokeTool?: (
    params: Record<string, unknown>,
    options?: { signal?: AbortSignal; onUpdate?: (result: AgentToolResult<unknown>) => void },
  ) => Promise<AgentToolResult<unknown>>;
};

function pathEntries(value: unknown): string[] {
  if (Array.isArray(value)) return value.flatMap(pathEntries);
  if (typeof value !== "string") return [];
  // Test the unsplit path first: commas and semicolons can be literal URL data.
  return [value, ...value.split(/[,;\n]/).map((path) => path.trim())];
}

/** Native bash owns the URL filesystem, even for URLs inside quoted words. */
export function bashHasInternalUrl(command: unknown, router: OmpInternalUrlRouter): boolean {
  if (typeof command !== "string") return false;
  // This is a conservative routing scan, not shell evaluation. Splitting quoted
  // text may over-delegate echo/heredoc content; native bash still executes it.
  return command
    .split(/[\s|&;()<>]+/)
    .some((word) => router.canHandle(word.replace(/^["'`]+|["'`]+$/g, "")));
}

function internalTarget(
  name: string,
  params: unknown,
  router: OmpInternalUrlRouter,
): string | undefined {
  const args = asRecord(params) ?? {};
  if (name === "bash") {
    for (const cwd of [args.cwd, args.workdir]) {
      if (typeof cwd === "string" && router.canHandle(cwd)) return cwd;
    }
    return bashHasInternalUrl(args.command, router) ? asString(args.command) : undefined;
  }
  const paths = name === "grep" ? [args.path, args.paths] : [args.path];
  for (const path of paths.flatMap(pathEntries)) {
    if (name === "read" || name === "grep") {
      if (router.canResolve(path)) return path;
    } else if (router.canHandle(path) && router.writeTarget(path)?.spec.write) {
      // Read-only schemes (notably issue/pr) retain AFT's own write semantics.
      // Handler-owned edit refusals remain the native edit tool's responsibility.
      return path;
    }
  }
  return undefined;
}

/** Accept native inputs before execute, without changing upstream Pi schemas. */
function ompParameters(name: string, original: TSchema): TSchema {
  const originalProperties = asRecord(asRecord(original)?.properties) ?? {};
  const properties = { ...originalProperties } as Record<string, TSchema>;
  if (name === "write") {
    properties.content = Type.Optional(
      Type.String({ description: "Full contents; may be omitted for proc://<id>/kill." }),
    );
  } else if (name === "edit") {
    // OMP offers replace, patch and input-only edit modes. AFT may itself be in
    // hashline mode (patch required, additionalProperties:false); neither schema
    // may reject a native URL edit before the host can handle it.
    for (const key of Object.keys(properties)) properties[key] = Type.Optional(properties[key]);
    properties.path = Type.Optional(Type.String());
    properties.input = Type.Optional(Type.String());
    properties.old_string = Type.Optional(Type.String());
    properties.new_string = Type.Optional(Type.String());
    properties.replace_all = Type.Optional(Type.Boolean());
    const aftEntry = asRecord(originalProperties.edits)?.items;
    properties.edits = Type.Optional(
      Type.Array(
        Type.Object({
          ...(asRecord(asRecord(aftEntry)?.properties) as Record<string, TSchema>),
          op: Type.Optional(
            Type.Union([Type.Literal("create"), Type.Literal("delete"), Type.Literal("update")]),
          ),
          rename: Type.Optional(Type.String()),
          diff: Type.Optional(Type.String()),
        }),
      ),
    );
  } else if (name === "grep") {
    properties.path = Type.Optional(Type.Union([Type.String(), Type.Array(Type.String())]));
    properties.paths = Type.Optional(Type.Array(Type.String()));
    properties.case = Type.Optional(Type.Boolean());
    properties.gitignore = Type.Optional(Type.Boolean());
    properties.skip = Type.Optional(Type.Union([Type.Number(), Type.Null()]));
  } else if (name === "bash") {
    properties.timeout = Type.Optional(Type.Union([Type.Number(), Type.String()]));
    properties.cwd = Type.Optional(Type.String());
    properties.pty = Type.Optional(Type.Boolean());
    properties.async = Type.Optional(Type.Boolean());
    properties.name = Type.Optional(Type.String({ maxLength: 48 }));
    properties.ready = Type.Optional(
      Type.Object({
        log: Type.Optional(Type.String()),
        port: Type.Optional(Type.Number()),
        host: Type.Optional(Type.String()),
        timeout: Type.Optional(Type.Number()),
      }),
    );
  }
  return Type.Object(properties);
}

function resultText(result: unknown): string {
  const content = asRecord(result)?.content;
  return Array.isArray(content)
    ? content
        .map((block) => asString(asRecord(block)?.text))
        .filter(Boolean)
        .join("\n")
    : "";
}

export function withOmpInternalUrls<TParams extends TSchema, TDetails, TState>(
  tool: ToolDefinition<TParams, TDetails, TState>,
  router: OmpInternalUrlRouter | undefined,
): ToolDefinition<TParams, TDetails, TState> {
  if (!router || !["read", "write", "edit", "grep", "bash"].includes(tool.name)) return tool;
  const delegatedResults = new WeakSet<object>();
  const remember = (result: AgentToolResult<unknown>) => {
    delegatedResults.add(result);
  };
  return {
    ...tool,
    ...(tool.name === "bash" ? { readsSkillUris: true } : {}),
    parameters: ompParameters(tool.name, tool.parameters) as TParams,
    prepareArguments(args) {
      // Native URL inputs must not undergo AFT's path/edit alias normalization.
      return (
        internalTarget(tool.name, args, router)
          ? args
          : tool.prepareArguments
            ? tool.prepareArguments(args)
            : args
      ) as Static<TParams>;
    },
    async execute(id, params, signal, onUpdate, context) {
      const native = context as OmpContext;
      if (typeof native.invokeTool === "function" && internalTarget(tool.name, params, router)) {
        const result = await native.invokeTool(params as Record<string, unknown>, {
          signal,
          ...(onUpdate
            ? {
                onUpdate: (update: AgentToolResult<unknown>) => {
                  remember(update);
                  onUpdate(update as AgentToolResult<TDetails>);
                },
              }
            : {}),
        });
        remember(result);
        return result as AgentToolResult<TDetails>;
      }
      const prepared = tool.prepareArguments ? tool.prepareArguments(params) : params;
      // The OMP schema is intentionally wider. Ordinary filesystem calls still
      // obey AFT's original schema and runtime validation (e.g. required content).
      if (!Value.Check(tool.parameters, prepared))
        throw new Error(`${tool.name}: invalid AFT filesystem parameters`);
      return tool.execute(id, prepared, signal, onUpdate, context);
    },
    renderResult(result, options = { expanded: false, isPartial: false }, theme, context) {
      const renderContext = context as RenderContextLike;
      const target = internalTarget(tool.name, renderContext.args, router);
      if (delegatedResults.has(result) || target) {
        const isError = renderContext.isError || asRecord(result)?.isError === true;
        const status = isError ? "error" : options.isPartial ? "running" : "completed";
        const summary = `${tool.name} ${target ?? ""} — OMP ${status}`;
        return collapsibleResult({
          summary: theme.fg(isError ? "error" : "muted", summary),
          full: new Text(`${summary}\n${resultText(result)}`, 0, 0),
          expanded: (options as RenderResultOptionsLike).expanded,
          context: renderContext,
        });
      }
      return tool.renderResult
        ? tool.renderResult(result, options, theme, context)
        : new Text(resultText(result), 0, 0);
    },
  };
}
