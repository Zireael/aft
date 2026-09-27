import { describe, expect, test } from "bun:test";
import { PassThrough, Writable } from "node:stream";
import {
  loadFeaturePlan,
  type NativeResult,
  type PlanFeature,
  type SetupPlan,
} from "../lib/feature-plan.js";
import {
  applySelectionRules,
  promptFeatureList,
  renderFeatureList,
} from "../setup/feature-list.js";
import {
  buildAnswers,
  explanationLines,
  featureListGroups,
  featureMode,
  groupRows,
  initialSelections,
  runFeatureSetup,
  runFeatureWizard,
  selectionRules,
  type WizardIO,
} from "../setup/feature-wizard.js";

function row(id: string, overrides: Partial<PlanFeature> = {}): PlanFeature {
  const kind = id.startsWith("indexes.")
    ? "index"
    : id.startsWith("github.")
      ? "capability"
      : id.startsWith("bash.")
        ? "setting"
        : "tool";
  const group =
    kind === "index"
      ? "Indexes"
      : kind === "capability"
        ? "GitHub"
        : kind === "setting" || id === "bash"
          ? "Shell"
          : "Search/navigation";
  return {
    id,
    kind,
    group,
    order: 0,
    label: id,
    description: `${id} description`,
    binding: {
      path: kind === "tool" ? "disabled_tools" : id,
      tool_name: kind === "tool" ? id : null,
    },
    default: kind !== "capability",
    configured: kind !== "capability",
    source: "default",
    proposed: kind !== "capability",
    effective: kind === "capability" ? "off" : "ready",
    reason: "default",
    available: true,
    unavailable_reason: null,
    cost_note: null,
    prerequisites: kind === "setting" ? ["bash"] : id === "github.write" ? ["github.read"] : [],
    ...overrides,
  };
}

function fixturePlan(overrides: Record<string, Partial<PlanFeature>> = {}): SetupPlan {
  const ids = [
    "grep",
    "glob",
    "aft_move",
    "aft_delete",
    "bash",
    "bash.compress",
    "bash.rewrite",
    "bash.background",
    "indexes.semantic",
    "github.read",
    "github.write",
  ];
  return {
    plan_version: 1,
    features: ids.map((id, index) => row(id, { order: index + 1, ...overrides[id] })),
  };
}

const BASH_SETTINGS = ["bash.compress", "bash.rewrite", "bash.background"];

/** Streams for driving the live prompt with key presses. */
function promptStreams() {
  let frame = "";
  const output = new Writable({
    write(chunk, _encoding, callback) {
      frame += chunk.toString();
      callback();
    },
  }) as Writable & { columns: number; rows: number; isTTY: boolean };
  output.columns = 100;
  output.rows = 80;
  output.isTTY = true;
  const input = new PassThrough() as PassThrough & { isTTY: boolean; setRawMode: () => void };
  input.isTTY = true;
  input.setRawMode = () => {};
  return { input, output, frame: () => frame };
}

const DOWN = "\x1b[B";

/**
 * Press keys in the live checklist for `plan` and return what it submits.
 * The cursor starts on the first group header; `moves` counts down-arrow
 * presses to reach the row to toggle, one entry per toggle, each counted from
 * where the previous toggle left the cursor.
 */
async function pressInChecklist(
  plan: SetupPlan,
  initial: string[],
  moves: number[],
): Promise<{ picked: string[]; frame: string }> {
  const streams = promptStreams();
  const done = promptFeatureList(
    "Choose",
    featureListGroups(plan),
    initial,
    () => {
      throw new Error("cancelled");
    },
    streams,
    selectionRules(plan),
  );
  for (const count of moves) {
    for (let index = 0; index < count; index += 1) streams.input.write(DOWN);
    streams.input.write(" ");
  }
  streams.input.write("\r");
  const picked = (await done).sort();
  // biome-ignore lint/suspicious/noControlCharactersInRegex: strip ANSI escapes
  return { picked, frame: streams.frame().replace(/\x1b\[[0-9;?]*[A-Za-z]/g, "") };
}

describe("selection rules", () => {
  test("come from the plan: bash settings require bash, GitHub write needs read", () => {
    const rules = selectionRules(fixturePlan());
    expect([...rules.requires]).toEqual(BASH_SETTINGS.map((id) => [id, "bash"]));
    expect([...rules.implies]).toEqual([["github.write", "github.read"]]);
    // A tool's index prerequisites never change the checklist.
    const indexed = selectionRules(fixturePlan({ grep: { prerequisites: ["indexes.semantic"] } }));
    expect(indexed.requires.has("grep")).toBe(false);
    expect(indexed.implies.has("grep")).toBe(false);
  });

  test("checking write checks read, and unchecking read unchecks write", () => {
    const rules = selectionRules(fixturePlan());
    expect(applySelectionRules([], ["github.write"], rules).sort()).toEqual([
      "github.read",
      "github.write",
    ]);
    expect(applySelectionRules(["github.read", "github.write"], ["github.write"], rules)).toEqual(
      [],
    );
    // Unchecking write alone leaves read as it is.
    expect(applySelectionRules(["github.read", "github.write"], ["github.read"], rules)).toEqual([
      "github.read",
    ]);
  });

  test("a bash setting cannot be toggled while bash is off, and keeps its value", () => {
    const rules = selectionRules(fixturePlan());
    const bashOff = ["bash.compress"];
    expect(applySelectionRules(bashOff, [], rules)).toEqual(["bash.compress"]);
    expect(applySelectionRules(bashOff, ["bash.compress", "bash.rewrite"], rules)).toEqual([
      "bash.compress",
    ]);
    // Unchecking bash keeps the settings' values for when it comes back.
    expect(applySelectionRules(["bash", "bash.compress"], ["bash.compress"], rules)).toEqual([
      "bash.compress",
    ]);
  });
});

describe("the live checklist", () => {
  const allOn = ["bash", ...BASH_SETTINGS];
  // Down-arrow counts from the first group header (Search/navigation) in the
  // fixture: 4 rows there, then Shell's header, bash and the three settings,
  // then Indexes' header and its row, then GitHub's header, read and write.
  const TO_BASH = 6;
  const TO_READ = 13;

  test("toggling bash off greys out its settings and they cannot be toggled", async () => {
    const plan = fixturePlan();
    // Uncheck bash, then press space on each of its three settings.
    const { picked, frame } = await pressInChecklist(plan, allOn, [TO_BASH, 1, 1, 1]);
    expect(picked).toEqual(BASH_SETTINGS.slice().sort());
    // Clack redraws only changed lines, so look for the redrawn rows anywhere
    // after the first frame.
    const redrawn = frame.slice(frame.indexOf("Enter: confirm"));
    for (const id of BASH_SETTINGS) {
      expect(redrawn).toContain(`◻ ${id} (not applicable: bash is off)`);
    }
    // The submitted summary names none of the settings.
    const summary = frame.slice(frame.lastIndexOf("◇  Choose"));
    for (const id of BASH_SETTINGS) expect(summary).not.toContain(id);
  });

  test("checking bash again brings its settings back as they were", async () => {
    const plan = fixturePlan();
    const { picked } = await pressInChecklist(plan, allOn, [TO_BASH, 0]);
    expect(picked).toEqual(allOn.slice().sort());
  });

  test("checking Write checks Read", async () => {
    const { picked } = await pressInChecklist(fixturePlan(), [], [TO_READ + 1]);
    expect(picked).toEqual(["github.read", "github.write"]);
  });

  test("unchecking Read clears Write", async () => {
    const { picked } = await pressInChecklist(
      fixturePlan(),
      ["github.read", "github.write"],
      [TO_READ],
    );
    expect(picked).toEqual([]);
  });

  test("the static frame draws settings of an unchecked tool as not applicable", () => {
    const plan = fixturePlan({ bash: { label: "bash" } });
    const frame = renderFeatureList(
      "Choose",
      featureListGroups(plan),
      new Set(["bash.compress"]),
      100,
      null,
      selectionRules(plan),
    );
    expect(frame).toContain("◻ bash.compress (not applicable: bash is off)");
    expect(frame).toContain("◻ Shell");
  });
});

describe("answers", () => {
  test("write both GitHub keys and every bash setting explicitly", () => {
    const plan = fixturePlan();
    const answers = buildAnswers(plan, {
      ...initialSelections(plan),
      "github.write": true,
      "github.read": true,
    });
    expect(answers.selections).toMatchObject({
      "github.read": true,
      "github.write": true,
      bash: true,
      "bash.compress": true,
      "bash.rewrite": true,
      "bash.background": true,
    });
    // Both off are sent as false, not left out.
    const off = buildAnswers(plan, initialSelections(plan)).selections;
    expect(off["github.read"]).toBe(false);
    expect(off["github.write"]).toBe(false);
  });

  test("a write-only pick sends read too", async () => {
    const plan = fixturePlan();
    const io: WizardIO = {
      selectRows: async (_message, _options, initial) => [...initial, "github.write"],
      info: () => {},
      note: () => {},
    };
    const answers = await runFeatureWizard(plan, io, () => "ready");
    expect(answers.selections["github.write"]).toBe(true);
    expect(answers.selections["github.read"]).toBe(true);
  });

  test("settings of an unchecked bash keep their remembered values", async () => {
    const plan = fixturePlan();
    const io: WizardIO = {
      selectRows: async () => ["bash.compress"],
      info: () => {},
      note: () => {},
    };
    const answers = await runFeatureWizard(plan, io, () => "ready");
    expect(answers.selections).toMatchObject({
      bash: false,
      "bash.compress": true,
      "bash.rewrite": false,
      "bash.background": false,
    });
  });

  test("no bash companion is ever sent, so none can land in disabled_tools", async () => {
    const plan = fixturePlan();
    for (const picked of [[], ["bash"], ["bash.compress"]]) {
      const io: WizardIO = {
        selectRows: async () => picked,
        info: () => {},
        note: () => {},
      };
      const answers = await runFeatureWizard(plan, io, () => "ready");
      for (const id of Object.keys(answers.selections)) {
        expect(id).not.toMatch(/^bash_/);
      }
    }
  });
});

describe("feature wizard rendering", () => {
  test("rows are grouped by the plan with separate grep and glob rows; GitHub is a checkbox group", () => {
    const groups = groupRows(fixturePlan());
    expect([...groups.keys()]).toEqual(["Search/navigation", "Shell", "Indexes", "GitHub"]);
    expect(groups.get("Search/navigation")?.map((feature) => feature.id)).toEqual([
      "grep",
      "glob",
      "aft_move",
      "aft_delete",
    ]);
    expect(groups.get("Shell")?.map((feature) => feature.id)).toEqual(["bash", ...BASH_SETTINGS]);
    expect(groups.get("GitHub")?.map((feature) => feature.id)).toEqual([
      "github.read",
      "github.write",
    ]);
  });

  test("explanations come from the plan, including costs and platform adjustments", () => {
    const plan = fixturePlan({
      "indexes.semantic": {
        cost_note: "may download an ONNX runtime and use CPU",
        proposed: false,
        unavailable_reason: "semantic_backend_unsupported_platform",
        effective: "unavailable",
      },
      aft_move: { description: "backed up for undo" },
      aft_delete: { description: "refuses symlinks" },
    });
    const lines = explanationLines(plan).join("\n");
    expect(lines).toContain("backed up for undo");
    expect(lines).toContain("refuses symlinks");
    expect(lines).toContain("may download an ONNX runtime and use CPU");
    expect(lines).toContain("semantic_backend_unsupported_platform");
  });

  test("an untouched save sends the proposed values for every row", async () => {
    const plan = fixturePlan({
      aft_move: { configured: false, proposed: false, effective: "off" },
      aft_delete: { configured: false, proposed: false, effective: "off" },
      "indexes.semantic": { proposed: false },
    });
    const io: WizardIO = {
      selectRows: async (_message, _options, initial) => initial,
      info: () => {},
      note: () => {},
    };
    const answers = await runFeatureWizard(plan, io);
    expect(answers).toEqual({
      plan_version: 1,
      selections: {
        grep: true,
        glob: true,
        aft_move: false,
        aft_delete: false,
        bash: true,
        "bash.compress": true,
        "bash.rewrite": true,
        "bash.background": true,
        "indexes.semantic": false,
        "github.read": false,
        "github.write": false,
      },
    });
  });

  test("a saved write shows both GitHub rows checked", async () => {
    const plan = fixturePlan({
      "github.read": { configured: false, proposed: true, effective: "ready" },
      "github.write": { configured: true, proposed: true, source: "config" },
    });
    let shown: string[] = [];
    const io: WizardIO = {
      selectRows: async (_message, _options, initial) => {
        shown = initial;
        return initial;
      },
      info: () => {},
      note: () => {},
    };
    const answers = await runFeatureWizard(plan, io, () => "ready");
    expect(shown).toContain("github.read");
    expect(shown).toContain("github.write");
    expect(answers.selections["github.read"]).toBe(true);
  });

  test("checking GitHub read warns once when gh is missing or signed out, and never blocks", async () => {
    for (const [status, needle] of [
      ["missing", "not on PATH"],
      ["signed_out", "gh auth login"],
    ] as const) {
      const warned: string[] = [];
      let checks = 0;
      const io: WizardIO = {
        selectRows: async (_message, _options, initial) => [...initial, "github.read"],
        info: () => {},
        warn: (message) => warned.push(message),
        note: () => {},
      };
      const answers = await runFeatureWizard(fixturePlan(), io, () => {
        checks += 1;
        return status;
      });
      expect(checks).toBe(1);
      expect(warned).toHaveLength(1);
      expect(warned[0]).toContain(needle);
      expect(answers.selections["github.read"]).toBe(true);
    }
  });

  test("no GitHub CLI check runs when GitHub read is left off", async () => {
    let checks = 0;
    const io: WizardIO = {
      selectRows: async (_message, _options, initial) => initial,
      info: () => {},
      note: () => {},
    };
    await runFeatureWizard(fixturePlan(), io, () => {
      checks += 1;
      return "missing";
    });
    expect(checks).toBe(0);
  });
});

describe("feature setup modes", () => {
  function recorder(result: Partial<NativeResult> = {}) {
    const calls: { args: string[]; input?: string }[] = [];
    const run = (args: string[], input?: string): NativeResult => {
      calls.push({ args, input });
      return { ok: true, stdout: "", stderr: "", status: 0, ...result };
    };
    return { calls, run };
  }

  test("modes are chosen from argv", () => {
    expect(featureMode(["--plan"])).toBe("plan");
    expect(featureMode(["--yes"])).toBe("yes");
    expect(featureMode(["--answers", "a.json"])).toBe("answers");
    expect(featureMode([])).toBe("interactive");
  });

  test("--yes and --answers together fail without calling the binary", async () => {
    const { calls, run } = recorder();
    let err = "";
    const code = await runFeatureSetup(["--yes", "--answers", "a.json"], {
      run,
      stderr: (text) => {
        err += text;
      },
    });
    expect(code).toBe(2);
    expect(err).toContain("mutually exclusive");
    expect(calls).toEqual([]);
  });

  test("--plan forwards only an explicit harness and prints the binary's stdout", async () => {
    const { calls, run } = recorder({ stdout: '{"plan_version":1,"features":[]}\n' });
    let out = "";
    await runFeatureSetup(["--plan"], { run, stdout: (text) => (out += text) });
    await runFeatureSetup(["--plan", "--harness", "omp"], { run, stdout: () => {} });
    expect(calls.map((call) => call.args)).toEqual([
      ["setup", "--plan"],
      ["setup", "--plan", "--harness", "omp"],
    ]);
    expect(out).toBe('{"plan_version":1,"features":[]}\n');
  });

  test("the interactive save writes answers once and does not repeat load warnings", async () => {
    const plan = fixturePlan();
    const calls: string[][] = [];
    const inputs: (string | undefined)[] = [];
    const run = (args: string[], input?: string): NativeResult => {
      calls.push(args);
      inputs.push(input);
      return args.includes("--plan")
        ? { ok: true, stdout: JSON.stringify(plan), stderr: "", status: 0 }
        : { ok: true, stdout: "{}", stderr: "", status: 0 };
    };
    const io: WizardIO = {
      selectRows: async (_m, _o, initial) => initial,
      info: () => {},
      note: () => {},
    };
    expect(await runFeatureSetup([], { run, io, interactive: true })).toBe(0);
    expect(calls).toEqual([
      ["setup", "--plan"],
      ["setup", "--answers", "-", "--no-load-warnings"],
    ]);
    expect(JSON.parse(inputs[1] ?? "{}").plan_version).toBe(1);
  });

  test("an unknown plan version is refused", () => {
    const loaded = loadFeaturePlan(null, () => ({
      ok: true,
      stdout: JSON.stringify({ plan_version: 2, features: [] }),
      stderr: "",
      status: 0,
    }));
    expect(loaded).toEqual({
      ok: false,
      error: "unsupported_setup_plan_version",
      configRejected: false,
    });
  });
});
