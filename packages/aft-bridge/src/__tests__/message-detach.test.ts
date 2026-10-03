import { describe, expect, test } from "bun:test";
import {
  shouldInterruptWaitsForMessage,
  standaloneDetachKeywordRanges,
  stripStandaloneDetachKeywords,
} from "../message-detach.js";

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

// The ranges must name exactly the characters the strip removes, or offsets a
// host moves with them would point at the wrong text.
describe("standaloneDetachKeywordRanges", () => {
  const removeRanges = (text: string, ranges: Array<[number, number]>) => {
    let result = "";
    let cursor = 0;
    for (const [start, end] of ranges) {
      result += text.slice(cursor, start);
      cursor = end;
    }
    return result + text.slice(cursor);
  };

  test("removing the ranges reproduces the strip", () => {
    for (const text of [
      "&detach",
      "&detach, continue",
      "before &detach middle &detach after",
      "&detach&detach",
      "(&detach) keep &detachment and x&detach",
      "no token here",
    ]) {
      expect(removeRanges(text, standaloneDetachKeywordRanges(text))).toBe(
        stripStandaloneDetachKeywords(text),
      );
    }
  });

  test("ranges start at the token, not at the boundary character before it", () => {
    expect(standaloneDetachKeywordRanges("go &detach now")).toEqual([[3, 10]]);
    expect(standaloneDetachKeywordRanges("&detach")).toEqual([[0, 7]]);
    expect(standaloneDetachKeywordRanges("&detachment")).toEqual([]);
  });
});
