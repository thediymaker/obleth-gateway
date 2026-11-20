import { describe, expect, it } from "vitest";
import { readFailed, readProblem } from "./read-state";

const NOW = 1_800_000_000_000;

describe("a live read's state", () => {
  it("says it can't read, never 'no traffic', when the first read failed", () => {
    const q = { isError: true, isPending: true, dataUpdatedAt: 0, data: undefined };
    expect(readFailed(q)).toBe(true);
    expect(readProblem(q, "the usage", NOW)).toBe("Can't read the usage right now. Retrying automatically.");
  });

  it("keeps the last answer and says how old it is when a refresh failed", () => {
    const q = { isError: true, isPending: false, dataUpdatedAt: NOW - 180_000, data: [] };
    expect(readFailed(q)).toBe(false);
    expect(readProblem(q, "the usage", NOW)).toBe("Can't refresh the usage. Last updated 3 min ago; retrying automatically.");
  });

  it("says nothing while loading, or when the read is current (an empty answer is not a failure)", () => {
    expect(readProblem({ isError: false, isPending: true, dataUpdatedAt: 0 }, "the usage")).toBeNull();
    expect(readProblem({ isError: false, isPending: false, dataUpdatedAt: NOW, data: [] }, "the usage")).toBeNull();
  });
});
