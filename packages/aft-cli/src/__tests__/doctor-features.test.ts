import { describe, expect, test } from "bun:test";
import { renderFeatureStatus } from "../doctor/features.js";
import type { PlanFeature, SetupPlan } from "../lib/feature-plan.js";

/** The v1 catalog's ids and groups, in plan order. */
const CATALOG: [string, string][] = [
  ["aft_search", "Search/navigation"],
  ["grep", "Search/navigation"],
  ["glob", "Search/navigation"],
  ["aft_outline", "Search/navigation"],
  ["aft_zoom", "Search/navigation"],
  ["aft_callgraph", "Search/navigation"],
  ["aft_inspect", "Search/navigation"],
  ["aft_conflicts", "Search/navigation"],
  ["read", "Editing"],
  ["write", "Editing"],
  ["edit", "Editing"],
  ["apply_patch", "Editing"],
  ["aft_import", "Editing"],
  ["aft_move", "Editing"],
  ["aft_delete", "Editing"],
  ["aft_safety", "Editing"],
  ["ast_grep_search", "Editing"],
  ["ast_grep_replace", "Editing"],
  ["bash", "Shell"],
  ["bash.compress", "Shell"],
  ["bash.rewrite", "Shell"],
  ["bash.background", "Shell"],
  ["indexes.trigram", "Indexes"],
  ["indexes.semantic", "Indexes"],
  ["indexes.callgraph", "Indexes"],
  ["github.read", "GitHub"],
  ["github.write", "GitHub"],
];

/** The wizard's labels for the rows whose label is not their id. */
const LABELS: Record<string, string> = {
  "bash.compress": "Output compression",
  "bash.rewrite": "Command rewrites",
  "bash.background": "Background commands",
  "github.read": "Read",
  "github.write": "Write",
};

/**
 * A plan the way the binary prints it for a migrated config that saved every
 * choice (so every row's source is "config"), with GitHub read turned on and
 * aft_move/aft_delete left at their default off.
 */
function migratedPlan(overrides: Record<string, Partial<PlanFeature>> = {}): SetupPlan {
  return {
    plan_version: 1,
    features: CATALOG.map(([id, group], index) => {
      const kind = id.startsWith("indexes.")
        ? "index"
        : id.startsWith("github.")
          ? "capability"
          : id.startsWith("bash.")
            ? "setting"
            : "tool";
      const defaultOn = kind !== "capability" && id !== "aft_move" && id !== "aft_delete";
      const on = id === "github.read" ? true : defaultOn;
      return {
        id,
        kind,
        group,
        order: index + 1,
        label: LABELS[id] ?? id,
        description: id,
        binding: { path: id, tool_name: kind === "tool" ? id : null },
        default: defaultOn,
        configured: on,
        source: "config",
        proposed: on,
        effective: !on ? "off" : kind === "index" ? "unavailable" : "ready",
        reason: "configured",
        available: kind !== "index",
        unavailable_reason: on && kind === "index" ? "runtime_not_observed" : null,
        cost_note: null,
        prerequisites: [],
        ...overrides[id],
      } satisfies PlanFeature;
    }),
  };
}

describe("doctor feature report", () => {
  test("GitHub read without gh is unavailable with the reason and the fix, never ready", () => {
    const report = renderFeatureStatus(migratedPlan(), { checkGh: () => "missing" });
    expect(report.problems).toEqual([
      {
        text: "github.read: unavailable — the GitHub CLI (gh) is not on PATH",
        remedy: "Install gh from https://cli.github.com, then run `gh auth login`.",
      },
    ]);
    expect(report.lines.join("\n")).not.toContain("ready");
    const signedOut = renderFeatureStatus(migratedPlan(), { checkGh: () => "signed_out" });
    expect(signedOut.problems[0]?.remedy).toBe("Run `gh auth login`.");
  });

  test("gh is checked only when GitHub read or write is on", () => {
    let checks = 0;
    const checkGh = () => {
      checks += 1;
      return "missing" as const;
    };
    const off = migratedPlan({
      "github.read": { configured: false, proposed: false, effective: "off", source: "default" },
    });
    expect(renderFeatureStatus(off, { checkGh }).problems).toEqual([]);
    expect(checks).toBe(0);
    expect(renderFeatureStatus(migratedPlan(), { checkGh: () => "ready" }).problems).toEqual([]);
  });

  test("git off names the affected features and the sidebar's remedy", () => {
    const report = renderFeatureStatus(migratedPlan(), {
      checkGh: () => "ready",
      git: { available: false, reason: "macos_developer_tools_missing", message: "…" },
    });
    expect(report.problems).toEqual([]);
    expect(report.notes).toHaveLength(1);
    const note = report.notes[0];
    expect(note?.text).toContain("git: off — macOS developer tools are not installed");
    expect(note?.remedy).toContain("aft_conflicts");
    expect(note?.remedy).toContain("`xcode-select --install`");
    expect(note?.remedy).toContain("Homebrew");
    expect(renderFeatureStatus(migratedPlan(), { git: { available: true } }).notes).toEqual([]);
  });

  test("the list fits one screen, says on/off, and never calls an unobserved index unavailable", () => {
    const { lines, problems } = renderFeatureStatus(migratedPlan(), { checkGh: () => "ready" });
    expect(problems).toEqual([]);
    expect(lines).toEqual([
      "Search/navigation: all 8 on",
      "Editing: 8 of 10 on; off: aft_move, aft_delete",
      "Shell: all 4 on",
      "Indexes: all 3 on",
      "GitHub: read on (config), write off",
      "Index build state is only visible inside a running session (the AFT sidebar or /aft-status).",
      "Every feature's full state as JSON: `npx @cortexkit/aft setup --plan`",
    ]);
    const text = lines.join("\n");
    expect(text).not.toMatch(/unavailable|runtime_not_observed|reason:|configured on/);
  });

  test("a value set in config that differs from the default is named with its source", () => {
    const { lines } = renderFeatureStatus(
      migratedPlan({
        aft_move: { configured: true, proposed: true, effective: "ready" },
        bash: { configured: false, proposed: false, effective: "off" },
      }),
    );
    expect(lines).toContain("Editing: 9 of 10 on; off: aft_delete; on: aft_move (config)");
    expect(lines).toContain("Shell: 3 of 4 on; off: bash (config)");
  });

  test("bash settings use the wizard's names and say when bash is off", () => {
    const compressOff = renderFeatureStatus(
      migratedPlan({ "bash.compress": { configured: false, proposed: false, effective: "off" } }),
    );
    expect(compressOff.lines).toContain("Shell: 3 of 4 on; off: Output compression (config)");
    const requiresBash = { effective: "off", reason: "requires bash" } as const;
    const bashOff = renderFeatureStatus(
      migratedPlan({
        bash: { configured: false, proposed: false, effective: "off" },
        "bash.compress": requiresBash,
        "bash.rewrite": requiresBash,
        "bash.background": requiresBash,
      }),
    );
    expect(bashOff.lines).toContain(
      "Shell: all 4 off (bash (config), Output compression (bash is off), Command rewrites (bash is off), Background commands (bash is off))",
    );
    expect(bashOff.lines.join("\n")).not.toMatch(/bash_(status|write|watch|kill)/);
  });

  test("read false written next to write true is explained in plain words", () => {
    const implied = {
      configured: false,
      proposed: true,
      effective: "ready",
      source: "config",
      reason: "implied by github.write",
    } as const;
    const report = renderFeatureStatus(
      migratedPlan({
        "github.read": implied,
        "github.write": { configured: true, proposed: true, effective: "ready" },
      }),
      { checkGh: () => "ready" },
    );
    expect(report.lines).toContain(
      "GitHub: read on (on because write needs it), write on (config)",
    );
    expect(report.problems).toEqual([]);
    expect(report.notes).toEqual([
      {
        text: "github.read is false in your config but github.write is true, so AFT turns GitHub read on anyway: posting a comment needs reading.",
        remedy:
          "Set github.read to true, or github.write to false, so the config says what AFT does.",
      },
    ]);
    // Read left out (an older write-only save) says why read is on, with no note.
    const absent = renderFeatureStatus(
      migratedPlan({
        "github.read": { ...implied, source: "default" },
        "github.write": { configured: true, proposed: true, effective: "ready" },
      }),
    );
    expect(absent.lines).toContain(
      "GitHub: read on (on because write needs it), write on (config)",
    );
    expect(absent.notes).toEqual([]);
  });

  test("a real unavailable cause is a problem", () => {
    const { problems } = renderFeatureStatus(
      migratedPlan({
        "indexes.semantic": { unavailable_reason: "semantic_backend_unsupported_platform" },
      }),
    );
    expect(problems).toEqual([
      { text: "indexes.semantic: unavailable — semantic_backend_unsupported_platform" },
    ]);
  });
});
