import { describe, it, expect } from "vitest";
import { timeAgo } from "./relative-time";

describe("timeAgo", () => {
  it("returns empty string for null", () => {
    expect(timeAgo(null)).toBe("");
  });
  it("formats recent times in seconds/minutes/hours", () => {
    const now = Date.now();
    expect(timeAgo(new Date(now - 5_000).toISOString())).toBe("5s ago");
    expect(timeAgo(new Date(now - 120_000).toISOString())).toBe("2m ago");
    expect(timeAgo(new Date(now - 7_200_000).toISOString())).toBe("2h ago");
  });
});
