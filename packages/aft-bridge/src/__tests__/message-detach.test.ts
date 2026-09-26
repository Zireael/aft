import { describe, expect, test } from "bun:test";
import { shouldInterruptWaitsForMessage } from "../message-detach.js";

// A new message interrupts a blocking wait while detach_on_user_message is on,
// whether or not the operator typed it; with the setting off only the
// standalone &detach token does.
describe("shouldInterruptWaitsForMessage", () => {
  test("any message interrupts while detach_on_user_message is on", () => {
    expect(shouldInterruptWaitsForMessage(true, "please continue")).toBe(true);
    // A machine-generated message carries no operator text.
    expect(shouldInterruptWaitsForMessage(true, "")).toBe(true);
  });

  test("with detach_on_user_message off only &detach interrupts", () => {
    expect(shouldInterruptWaitsForMessage(false, "please continue")).toBe(false);
    expect(shouldInterruptWaitsForMessage(false, "")).toBe(false);
    expect(shouldInterruptWaitsForMessage(false, "&detach")).toBe(true);
    expect(shouldInterruptWaitsForMessage(false, "please &detach now")).toBe(true);
    expect(shouldInterruptWaitsForMessage(false, "document &detachment")).toBe(false);
  });
});
