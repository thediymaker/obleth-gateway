import { describe, it, expect } from "vitest";
import { sse } from "./sse";

describe("sse", () => {
  it("frames an event with JSON data", () => {
    expect(sse("token", { text: "hi" })).toBe('event: token\ndata: {"text":"hi"}\n\n');
  });
});
