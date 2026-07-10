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

  it("computes decode tok/s from the post-TTFB window, p50 and p10", () => {
    // 20 out tokens over (600 - 100) = 500 ms decode → 40 tok/s each.
    const s = summarizeStep(2, [
      { status: 200, ttfbMs: 100, totalMs: 600, inTokens: 10, outTokens: 20 },
      { status: 200, ttfbMs: 100, totalMs: 600, inTokens: 10, outTokens: 20 },
    ], 2);
    expect(s.p50DecodeTps).toBeCloseTo(40, 3);
    expect(s.p10DecodeTps).toBeCloseTo(40, 3);
    expect(s.tokensPerS).toBeCloseTo(20, 3); // 40 tokens / 2 s aggregate — unchanged
  });

  it("skips decode samples for zero out-tokens and zero decode time", () => {
    const s = summarizeStep(1, [
      { status: 200, ttfbMs: 10, totalMs: 20, inTokens: 10, outTokens: 0 },  // no tokens
      { status: 200, ttfbMs: 20, totalMs: 20, inTokens: 10, outTokens: 4 },  // no window
    ], 1);
    expect(s.p50DecodeTps).toBe(0);
    expect(s.p10DecodeTps).toBe(0);
    expect(s.tokensPerS).toBeCloseTo(4, 3); // aggregate still counts arrived tokens
  });
});
