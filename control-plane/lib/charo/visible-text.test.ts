import { describe, expect, it } from "vitest";
import { splitHiddenReasoning, stripHiddenReasoning } from "./visible-text";

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

describe("splitHiddenReasoning", () => {
  it("keeps the thinking it strips from the answer", () => {
    expect(splitHiddenReasoning("<think>Check 17 and 23.</think>No, 391 = 17 × 23.")).toEqual({
      visible: "No, 391 = 17 × 23.",
      thinking: "Check 17 and 23.",
    });
  });

  it("treats a block still open as thinking in progress", () => {
    expect(splitHiddenReasoning("<think>Still working it out")).toEqual({ visible: "", thinking: "Still working it out" });
  });

  it("reads a leading orphan close as thinking, from a template that opened <think> itself", () => {
    expect(splitHiddenReasoning("First, factor it.</think>\n\nIt is composite.")).toEqual({
      visible: "It is composite.",
      thinking: "First, factor it.",
    });
  });

  it("joins several blocks and leaves plain text alone", () => {
    expect(splitHiddenReasoning("<think>a</think>One. <think>b</think>Two.")).toEqual({ visible: "One. Two.", thinking: "ab" });
    expect(splitHiddenReasoning("Just an answer.")).toEqual({ visible: "Just an answer.", thinking: "" });
  });
});
