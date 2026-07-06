import { describe, it, expect } from "vitest";
import { resultRenderer } from "./registry";

describe("resultRenderer", () => {
  it("returns a component for a known type", () => {
    expect(typeof resultRenderer("bench_result")).toBe("function");
  });
  it("returns the fallback for an unknown type (never undefined)", () => {
    expect(typeof resultRenderer("nope")).toBe("function");
  });
});
