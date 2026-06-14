import { describe, expect, test } from "bun:test";
import { isRustZoomBatchEnvelope, unwrapRustZoomBatchEnvelope } from "../zoom-format.js";

describe("Rust zoom batch envelope", () => {
  test("isRustZoomBatchEnvelope accepts valid batch shape", () => {
    const response = {
      success: true,
      complete: true,
      symbols: [
        { name: "a", response: { success: true, name: "a", content: "x" } },
        { name: "b", response: { success: false, message: "nope" } },
      ],
    };
    expect(isRustZoomBatchEnvelope(response)).toBe(true);
    expect(unwrapRustZoomBatchEnvelope(response)).toEqual({
      names: ["a", "b"],
      responses: [
        { success: true, name: "a", content: "x" },
        { success: false, message: "nope" },
      ],
    });
  });

  test("isRustZoomBatchEnvelope rejects single-symbol zoom shape", () => {
    expect(
      isRustZoomBatchEnvelope({
        success: true,
        name: "foo",
        content: "body",
      }),
    ).toBe(false);
  });
});
