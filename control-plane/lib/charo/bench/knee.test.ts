import { describe, it, expect } from "vitest";
import { detectKnee } from "./knee";
import type { StepOutcome } from "./types";

const step = (
  concurrency: number,
  p99: number,
  reqPerS: number,
  errorRate = 0,
): StepOutcome => ({
  concurrency, completed: 100, rejected: 0, errors: Math.round(errorRate * 100), errorRate,
  p50TtfbMs: p99 / 2, p90TtfbMs: p99, p99TtfbMs: p99, p50TotalMs: p99, p99TotalMs: p99,
  reqPerS, tokensPerS: reqPerS * 10,
  p50DecodeTps: 0, p10DecodeTps: 0,
});

// The ramp that motivated this rule: a 31B model on real hardware. TTFT rises
// steeply off the single-stream floor (batching queues prefill) but throughput
// never stops climbing, so there is no knee here.
const SCALING_RAMP = [
  step(1, 634, 0.5),
  step(5, 3488, 1.2),
  step(10, 3708, 2.5),
  step(20, 3972, 4.8),
  step(40, 5270, 6.8),
];

describe("detectKnee", () => {
  it("reports no knee when throughput keeps climbing, however far TTFT rose", () => {
    expect(detectKnee(SCALING_RAMP)).toEqual({
      concurrency: 40,
      confirmed: false,
      reason: null,
    });
  });

  it("does not call a TTFT jump a knee while throughput is still growing", () => {
    // p99 goes 634 -> 3488 (5.5x) but req/s more than doubles: that is batching
    // absorbing load, not saturation.
    const v = detectKnee([step(1, 634, 0.5), step(5, 3488, 1.2)]);
    expect(v.concurrency).toBe(5);
    expect(v.confirmed).toBe(false);
  });

  it("stops at the last clean step when latency climbs while throughput flattens", () => {
    // At 20: p99 2x the previous step, req/s up only 5%.
    const v = detectKnee([step(1, 100, 1), step(5, 150, 5), step(10, 200, 10), step(20, 400, 10.5)]);
    expect(v.concurrency).toBe(10);
    expect(v.confirmed).toBe(true);
    expect(v.reason).toMatch(/throughput/i);
  });

  it("stops at the last clean step when the error rate crosses 1%", () => {
    const v = detectKnee([step(1, 100, 1), step(5, 120, 5, 0.05)]);
    expect(v.concurrency).toBe(1);
    expect(v.confirmed).toBe(true);
    expect(v.reason).toMatch(/error rate/i);
  });

  it("reports no sustainable concurrency when the first step is already dirty", () => {
    const v = detectKnee([step(1, 100, 1, 0.2)]);
    expect(v.concurrency).toBeNull();
    expect(v.confirmed).toBe(false);
    expect(v.reason).toMatch(/error rate/i);
  });

  it("returns an empty verdict for an empty ramp", () => {
    expect(detectKnee([])).toEqual({ concurrency: null, confirmed: false, reason: null });
  });

  it("orders steps before walking them", () => {
    const v = detectKnee([step(10, 200, 10), step(1, 100, 1), step(5, 150, 5)]);
    expect(v.concurrency).toBe(10);
  });
});
