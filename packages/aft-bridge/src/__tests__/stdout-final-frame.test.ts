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
