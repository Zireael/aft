import { expect, test } from "bun:test";
import { appendBashStreamTail } from "../tools/bash.js";

test("bash preview scans a bounded tail rather than all prior output", () => {
  let tail = "";
  let charsScanned = 0;
  const chunk = `${"x".repeat(1023)}\n`;
  for (let i = 0; i < 1000; i++) {
    tail = appendBashStreamTail(tail, chunk);
    charsScanned += tail.length;
    expect(tail.length).toBeLessThanOrEqual(64 * 1024);
  }
  expect(charsScanned).toBe(63_471_616);
  expect(tail).toBe(chunk.repeat(64));
  expect(appendBashStreamTail(tail, `${"y".repeat(100_000)}\nlast\n`)).toBe(
    `${"y".repeat(65530)}\nlast\n`,
  );
});
