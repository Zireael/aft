import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// Pi supplies these modules to every extension through its own module mapping
// (Pi's docs/packages.md, "Declare dependencies"). A copy installed from our
// `dependencies` can bypass that mapping and load a second instance of the
// host's classes and registries, so Pi warns at startup when it sees one. They
// must be `"*"` peers instead, and kept external in the bundle.
const HOST_PROVIDED = [
  "@earendil-works/pi-ai",
  "@earendil-works/pi-agent-core",
  "@earendil-works/pi-coding-agent",
  "@earendil-works/pi-tui",
  "typebox",
];

interface Manifest {
  dependencies?: Record<string, string>;
  peerDependencies?: Record<string, string>;
  scripts?: Record<string, string>;
}

const manifest: Manifest = JSON.parse(
  readFileSync(join(import.meta.dir, "..", "..", "package.json"), "utf8"),
);

describe("package manifest", () => {
  test("never lists a Pi host-provided package in dependencies", () => {
    const dependencies = Object.keys(manifest.dependencies ?? {});
    expect(dependencies.filter((name) => HOST_PROVIDED.includes(name))).toEqual([]);
  });

  test("declares every host package the plugin imports as a * peer and keeps it external", () => {
    const build = manifest.scripts?.build ?? "";
    for (const name of [
      "@earendil-works/pi-ai",
      "@earendil-works/pi-coding-agent",
      "@earendil-works/pi-tui",
      "typebox",
    ]) {
      expect(manifest.peerDependencies?.[name]).toBe("*");
      expect(build).toContain(`--external ${name}`);
    }
  });
});
