import { describe, expect, test } from "bun:test";
import { BinaryBridge } from "../bridge.js";

describe("BinaryBridge stdout framing", () => {
  test("parses final push frame without trailing newline when stdout flushes", () => {
    const completions: unknown[] = [];
    const bridge = new BinaryBridge(
      "/tmp/aft-does-not-need-to-exist",
      process.cwd(),
      {
        onBashCompletion: (completion) => {
          completions.push(completion);
        },
      },
      { harness: "test" },
    );

    (bridge as any).onStdoutData(
      JSON.stringify({
        type: "bash_completed",
        task_id: "task-final",
        session_id: "s1",
        status: "completed",
        exit_code: 0,
        command: "echo done",
      }),
    );
    (bridge as any).flushStdoutBuffer();

    expect(completions).toHaveLength(1);
    expect((completions[0] as { task_id?: string }).task_id).toBe("task-final");
  });

  test("parses many complete stdout lines with trailing partial carryover", () => {
    const completions: unknown[] = [];
    const bridge = new BinaryBridge(
      "/tmp/aft-does-not-need-to-exist",
      process.cwd(),
      {
        onBashCompletion: (completion) => {
          completions.push(completion);
        },
      },
      { harness: "test" },
    );

    const completeLines = Array.from({ length: 5_000 }, (_, i) =>
      JSON.stringify({
        type: "bash_completed",
        task_id: `task-${i}`,
        session_id: "s1",
        status: "completed",
        exit_code: 0,
        command: "echo done",
      }),
    ).join("\n");
    const trailing = JSON.stringify({
      type: "bash_completed",
      task_id: "task-tail",
      session_id: "s1",
      status: "completed",
      exit_code: 0,
      command: "echo tail",
    });

    (bridge as any).onStdoutData(`${completeLines}\n${trailing.slice(0, 17)}`);
    expect(completions).toHaveLength(5_000);

    (bridge as any).onStdoutData(`${trailing.slice(17)}\n`);
    expect(completions).toHaveLength(5_001);
    expect((completions[completions.length - 1] as { task_id?: string }).task_id).toBe("task-tail");
  });
});

describe("BinaryBridge stdout scan cost", () => {
  // A large response arrives as many pipe chunks before its newline. Each
  // character must be examined for "\n" once; rescanning the pending line on
  // every chunk made a multi-megabyte response quadratic in its length.
  test("a line split across many chunks is scanned once, not once per chunk", () => {
    const completions: Array<{ task_id?: string; command?: string }> = [];
    const bridge = new BinaryBridge(
      "/tmp/aft-does-not-need-to-exist",
      process.cwd(),
      {
        onBashCompletion: (completion) => {
          completions.push(completion as { task_id?: string; command?: string });
        },
      },
      { harness: "test" },
    );

    const payload = "x".repeat(4 * 1024 * 1024);
    const line = `${JSON.stringify({
      type: "bash_completed",
      task_id: "task-large",
      session_id: "s1",
      status: "completed",
      exit_code: 0,
      command: payload,
    })}\n`;
    const chunkSize = 64 * 1024;
    for (let offset = 0; offset < line.length; offset += chunkSize) {
      (bridge as any).onStdoutData(line.slice(offset, offset + chunkSize));
    }

    expect(completions).toHaveLength(1);
    expect(completions[0]?.task_id).toBe("task-large");
    expect(completions[0]?.command?.length).toBe(payload.length);
    // Linear: every delivered character is scanned exactly once.
    expect((bridge as any).stdoutScannedChars).toBe(line.length);
  });

  test("lines that end mid-chunk keep the remainder for the next chunk", () => {
    const completions: Array<{ task_id?: string }> = [];
    const bridge = new BinaryBridge(
      "/tmp/aft-does-not-need-to-exist",
      process.cwd(),
      {
        onBashCompletion: (completion) => {
          completions.push(completion as { task_id?: string });
        },
      },
      { harness: "test" },
    );
    const frame = (id: string) =>
      JSON.stringify({
        type: "bash_completed",
        task_id: id,
        session_id: "s1",
        status: "completed",
        exit_code: 0,
        command: "c",
      });
    const stream = `  ${frame("a")}  \r\n\n${frame("b")}\n${frame("c")}`;
    // Feed one character at a time: the worst case for carry-over bookkeeping.
    for (const ch of stream) (bridge as any).onStdoutData(ch);
    expect(completions.map((c) => c.task_id)).toEqual(["a", "b"]);
    (bridge as any).flushStdoutBuffer();
    expect(completions.map((c) => c.task_id)).toEqual(["a", "b", "c"]);
  });
});
