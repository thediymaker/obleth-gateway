import { describe, it, expect } from "vitest";
import { resultRenderer } from "./registry";

describe("resultRenderer", () => {
  it("returns a component for a known type", () => {
    expect(typeof resultRenderer("bench_result")).toBe("function");
  });
  it("returns the fallback for an unknown type (never undefined)", () => {
    expect(typeof resultRenderer("nope")).toBe("function");
  });
  it("returns a dedicated component for mcp_test_result (not the fallback)", () => {
    expect(resultRenderer("mcp_test_result")).not.toBe(resultRenderer("definitely-unknown"));
  });
});
