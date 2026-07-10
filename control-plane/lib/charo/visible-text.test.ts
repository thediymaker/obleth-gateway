import { describe, expect, it } from "vitest";
import { stripHiddenReasoning } from "./visible-text";

describe("stripHiddenReasoning", () => {
  it("removes complete think blocks", () => {
    expect(stripHiddenReasoning("<think>I should not be shown.</think>Final answer.")).toBe("Final answer.");
  });

  it("holds an in-progress think block", () => {
    expect(stripHiddenReasoning("<think>I am still thinking")).toBe("");
  });

  it("removes leading orphan thought text when only the closing tag arrives", () => {
    const leaked =
      "` after my thought, then the tool output. I will summarize briefly.</think>\n" +
      "SearXNG is up and responding.";
    expect(stripHiddenReasoning(leaked)).toBe("SearXNG is up and responding.");
  });

  it("preserves ordinary text around a think block", () => {
    expect(stripHiddenReasoning("Before. <think>hide this</think> After.")).toBe("Before.  After.");
  });
});
