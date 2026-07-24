/// <reference path="../../bun-test.d.ts" />
import { mock } from "bun:test";

/**
 * Shared logger mock helper for OpenCode plugin tests.
 *
 * `logger.ts` exports a large surface (log/debug/warn/error, their
 * session-prefixed variants, getLogFilePath, and bridgeLogger). Bun's
 * `mock.module()` leaks partial mocks across test files, so every test
 * that imports `logger.js` must provide a complete mock shape. Use
 * {@link createLoggerMock} to centralize that shape and only override the
 * functions your test needs to spy on.
 *
 * @example
 *   import { createLoggerMock } from "./helpers/logger-mock.js";
 *
 *   const logMock = mock(() => {});
 *   const warnMock = mock(() => {});
 *   mock.module("../logger.js", () =>
 *     createLoggerMock({ log: logMock, warn: warnMock }),
 *   );
 */

export type LoggerMockFn = ReturnType<typeof mock<() => void>>;

export interface LoggerMock {
  log: LoggerMockFn;
  debug: LoggerMockFn;
  warn: LoggerMockFn;
  error: LoggerMockFn;
  sessionLog: LoggerMockFn;
  sessionDebug: LoggerMockFn;
  sessionWarn: LoggerMockFn;
  sessionError: LoggerMockFn;
  getLogFilePath: () => string;
  bridgeLogger: {
    log: () => void;
    warn: () => void;
    error: () => void;
    getLogFilePath: () => string;
  };
}

export type LoggerMockOverrides = Partial<LoggerMock>;

const noop = () => {};

function createDefaultBridgeLogger(): LoggerMock["bridgeLogger"] {
  return {
    log: noop,
    warn: noop,
    error: noop,
    getLogFilePath: () => "",
  };
}

/**
 * Build a complete mock of `../logger.js`.
 *
 * Pass spy mocks via `overrides` for the functions you want to assert on.
 * Any function not overridden returns a Bun mock that does nothing.
 */
export function createLoggerMock(overrides: LoggerMockOverrides = {}): LoggerMock {
  return {
    log: mock(() => {}),
    debug: mock(() => {}),
    warn: mock(() => {}),
    error: mock(() => {}),
    sessionLog: mock(() => {}),
    sessionDebug: mock(() => {}),
    sessionWarn: mock(() => {}),
    sessionError: mock(() => {}),
    getLogFilePath: () => "",
    bridgeLogger: createDefaultBridgeLogger(),
    ...overrides,
  };
}
