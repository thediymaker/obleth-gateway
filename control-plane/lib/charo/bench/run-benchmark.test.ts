import { describe, it, expect, vi } from "vitest";
import { runBenchmarkTool } from "./run-benchmark";

describe("runBenchmarkTool.parseArgs", () => {
  it("applies defaults and clamps steps to the concurrency cap", () => {
    const args = runBenchmarkTool.parseArgs({ model: "m" });
    expect(args.model).toBe("m");
    expect(args.steps).toEqual([1, 5, 10, 20, 40]);
    expect(args.stepDurationS).toBe(10);
    expect(args.inputTokens).toBe(256);
    expect(args.maxTokens).toBe(64);
  });
  it("rejects a missing model", () => {
    expect(() => runBenchmarkTool.parseArgs({})).toThrow(/model/i);
  });
});
