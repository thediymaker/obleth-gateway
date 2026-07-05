import { describe, it, expect } from "vitest";
import { ToolCallAccumulator } from "./tool-call-accumulator";

describe("ToolCallAccumulator", () => {
  it("concatenates name + arguments fragments by index", () => {
    const acc = new ToolCallAccumulator();
    acc.addDelta([{ index: 0, id: "c1", function: { name: "run_", arguments: '{"mo' } }]);
    acc.addDelta([{ index: 0, function: { name: "benchmark", arguments: 'del":"m"}' } }]);
    expect(acc.complete()).toEqual([{ name: "run_benchmark", arguments: '{"model":"m"}' }]);
  });

  it("handles two parallel tool calls by index", () => {
    const acc = new ToolCallAccumulator();
    acc.addDelta([{ index: 0, function: { name: "a", arguments: "{}" } }, { index: 1, function: { name: "b", arguments: "{}" } }]);
    expect(acc.complete().map((c) => c.name)).toEqual(["a", "b"]);
  });

  it("ignores non-array deltas", () => {
    const acc = new ToolCallAccumulator();
    acc.addDelta(undefined);
    acc.addDelta({});
    expect(acc.complete()).toEqual([]);
  });
});
