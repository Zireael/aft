/// <reference path="../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { __test__ } from "../index.js";

describe("Pi index audit wiring", () => {
  test("uses the emitting bridge directory for pushed bash callbacks", () => {
    expect(__test__.bridgeDirectoryFromCallback({ cwd: "/bridge/project" }, "/fallback")).toBe(
      "/bridge/project",
    );
    expect(__test__.bridgeDirectoryFromCallback({ cwd: "" }, "/fallback")).toBe("/fallback");
    expect(__test__.bridgeDirectoryFromCallback({}, "/fallback")).toBe("/fallback");
  });

  test("wires pattern-match pushes and unregisters process shutdown cleanup", () => {
    const source = readFileSync(new URL("../index.ts", import.meta.url), "utf8");

    expect(source).toContain("onBashPatternMatch");
    expect(source).toContain("handlePushedPatternMatch");
    expect(source).toContain("bridgeDirectoryFromCallback(bridge, process.cwd())");
    expect(source).not.toContain("directory: process.cwd()");
    expect(source).toContain("const unregisterShutdownCleanup = registerShutdownCleanup");
    expect(source).toContain("unregisterShutdownCleanup();");
  });
});
