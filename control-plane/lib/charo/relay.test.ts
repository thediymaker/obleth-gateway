import { describe, it, expect } from "vitest";
import { deltaText, deltaToolCalls, finishReason } from "./relay";

const chunk = (choice: unknown) => ({ choices: [choice] });

describe("relay chunk parsing", () => {
  it("pulls token text", () => {
    expect(deltaText(chunk({ delta: { content: "hi" } }))).toBe("hi");
    expect(deltaText(chunk({ delta: {} }))).toBe("");
  });
  it("pulls tool_calls delta array", () => {
    expect(deltaToolCalls(chunk({ delta: { tool_calls: [{ index: 0 }] } }))).toEqual([{ index: 0 }]);
    expect(deltaToolCalls(chunk({ delta: {} }))).toBeUndefined();
  });
  it("reads finish_reason", () => {
    expect(finishReason(chunk({ finish_reason: "tool_calls" }))).toBe("tool_calls");
    expect(finishReason(chunk({}))).toBeUndefined();
  });
});
