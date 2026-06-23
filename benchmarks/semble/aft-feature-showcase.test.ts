import { describe, expect, it } from "bun:test";
import { resolve } from "path";
import {
  parseArgs,
  renderReport,
  type ShowcaseReport,
} from "./aft-feature-showcase";

describe("aft-feature-showcase", () => {
  it("parses user-facing benchmark options", () => {
    const config = parseArgs([
      "--binary",
      "D:/aft/aft.exe",
      "--project-root",
      "D:/repo",
      "--query",
      "CandidateEntry",
      "--expected-file",
      "src/lib.rs",
      "--top-k",
      "7",
      "--no-color",
    ]);

    expect(config.binary).toBe("D:/aft/aft.exe");
    expect(config.projectRoot).toBe(resolve("D:/repo"));
    expect(config.query).toBe("CandidateEntry");
    expect(config.expectedFile).toBe("src/lib.rs");
    expect(config.topK).toBe(7);
    expect(config.color).toBe(false);
  });

  it("renders a polished report with quality, speed, and feature explanations", () => {
    const report: ShowcaseReport = {
      generatedAt: "2026-06-22T18:30:00.000Z",
      binary: "aft.exe",
      projectRoot: "D:/repo",
      query: "CandidateEntry",
      expectedFile: "src/lib.rs",
      topK: 5,
      comparisons: [
        {
          label: "Baseline grep",
          command: "grep",
          status: "ok",
          latencyMs: 50,
          resultCount: 5,
          expectedRank: 4,
          topFile: "src/other.rs",
          qualityNotes: ["fast literal fallback"],
        },
        {
          label: "RI v2 semantic_search",
          command: "semantic_search",
          status: "ok",
          latencyMs: 20,
          resultCount: 5,
          expectedRank: 1,
          topFile: "src/lib.rs",
          speedupVsBaseline: 2.5,
          qualityDeltaVsBaseline: 3,
          searchPlanIntent: "ExactSymbol",
          activeSafetyLane: "TrigramBody",
          laneCount: 3,
          rankingFeatures: ["exact_definition_boost"],
          enrichmentStates: ["enriched"],
          qualityNotes: ["definition-aware ranking"],
        },
      ],
      featureCards: [
        {
          title: "SearchPlan",
          status: "available",
          whyItMatters: "Shows which retrieval lanes were active.",
          evidence: ["intent ExactSymbol", "3 lanes"],
        },
      ],
      diagnostics: [
        {
          label: "Context pack",
          status: "ok",
          latencyMs: 12,
          summary: "2 packed items",
          whyItMatters: "Turns retrieval into usable context.",
        },
      ],
      recommendations: ["Use RI v2 for symbol-heavy code search."],
    };

    const rendered = renderReport(report, { color: false });

    expect(rendered).toContain("AFT Feature Showcase");
    expect(rendered).toContain("RI v2 semantic_search");
    expect(rendered).toContain("Expected file rank: #1");
    expect(rendered).toContain("2.50x faster than baseline");
    expect(rendered).toContain("exact_definition_boost");
    expect(rendered).toContain("Why it matters");
  });
});
