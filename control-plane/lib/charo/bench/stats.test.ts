import { describe, it, expect } from "vitest";
import { percentile, summarizeStep } from "./stats";

describe("percentile (nearest-rank, ported from obench stats.rs)", () => {
  it("matches obench test vectors", () => {
    expect(percentile([10, 20, 30, 40, 50], 50)).toBe(30);
    expect(percentile([10, 20, 30, 40, 50], 99)).toBe(50);
    expect(percentile([], 50)).toBe(0);
  });
});

describe("summarizeStep", () => {
  const ok = (ttfb: number, total: number) => ({ status: 200, ttfbMs: ttfb, totalMs: total, inTokens: 10, outTokens: 20 });

  it("counts completions and rates, 429 rejected not error", () => {
    const s = summarizeStep(5, [...Array(90)].map(() => ok(10, 20)).concat(
      [...Array(8)].map(() => ({ status: 429, ttfbMs: 0, totalMs: 0, inTokens: 0, outTokens: 0 })),
      [...Array(2)].map(() => ({ status: 500, ttfbMs: 0, totalMs: 0, inTokens: 0, outTokens: 0 })),
    ), 10);
    expect(s.completed).toBe(90);
    expect(s.rejected).toBe(8);
    expect(s.errors).toBe(2);
    expect(s.errorRate).toBeCloseTo(2 / 100, 5); // errors / attempts
    expect(s.reqPerS).toBeCloseTo(9, 5);         // completed / elapsed
    expect(s.p50TtfbMs).toBe(10);
  });

  it("empty step yields zeros, not NaN", () => {
    const s = summarizeStep(1, [], 10);
    expect(s.errorRate).toBe(0);
    expect(s.reqPerS).toBe(0);
    expect(s.p99TtfbMs).toBe(0);
  });
});
