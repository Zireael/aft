import { expect, test } from "bun:test";
import { BinaryBridge } from "../bridge.js";
import { RotatingLogSink } from "../durable-log.js";

test("newline-free stderr retains at most 64KiB and discloses truncation", () => {
  const bridge = new BinaryBridge("/fake/aft", process.cwd(), { maxRestarts: 0 });
  const probe = bridge as unknown as { onStderrData(data: string): void; stderrBuffer: string; stderrTail: string[]; logVia(message: string): void };
  probe.logVia = () => {};
  for (let i = 0; i < 2048; i++) probe.onStderrData("x".repeat(1024));
  console.log(`stderr retained chars=${probe.stderrBuffer.length}`);
  expect(probe.stderrBuffer.length).toBeLessThanOrEqual(65536);
  expect(probe.stderrTail.join("\n")).toContain("truncated");
});

test("slow durable sink bounds queued bytes and records dropped bytes", async () => {
  const sink = new RotatingLogSink("/unused-by-test");
  let release!: () => void;
  const blocked = new Promise<void>((resolve) => { release = resolve; });
  const writes: string[] = [];
  const probe = sink as unknown as { write(data: string): Promise<void> };
  probe.write = async (data) => { await blocked; writes.push(data); };
  for (let i = 0; i < 2048; i++) sink.append("x".repeat(1024));
  release();
  await sink.drain();
  const bytes = writes.reduce((total, line) => total + Buffer.byteLength(line), 0);
  console.log(`durable admitted bytes=${bytes}`);
  expect(bytes).toBeLessThan(1100000);
  expect(writes.join("")).toContain("dropped 1048576 bytes");
});
