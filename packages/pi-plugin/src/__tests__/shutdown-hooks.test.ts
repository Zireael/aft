/// <reference path="../bun-test.d.ts" />

import { afterEach, describe, expect, test } from "bun:test";
import {
  __resetShutdownCleanupsForTests,
  __shutdownCleanupCountForTests,
  registerShutdownCleanup,
} from "../shutdown-hooks.js";

afterEach(() => {
  __resetShutdownCleanupsForTests();
});

describe("Pi shutdown hooks", () => {
  test("returned unregister removes the process-global cleanup", () => {
    __resetShutdownCleanupsForTests();
    const unregister = registerShutdownCleanup(() => undefined);

    expect(__shutdownCleanupCountForTests()).toBe(1);
    unregister();
    expect(__shutdownCleanupCountForTests()).toBe(0);
  });
});
