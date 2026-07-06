import { describe, it, expect } from "vitest";
import { detectKnee, kneeConfirmed } from "./knee";
import type { StepOutcome } from "./types";

const step = (concurrency: number, p99: number, errorRate = 0): StepOutcome => ({
  concurrency, completed: 100, rejected: 0, errors: 0, errorRate,
  p50TtfbMs: p99 / 2, p90TtfbMs: p99, p99TtfbMs: p99, p50TotalMs: p99, p99TotalMs: p99,
  reqPerS: concurrency, tokensPerS: concurrency * 10,
});

describe("detectKnee", () => {
  it("returns the highest step within 4x baseline p99 and clean", () => {
    // baseline p99 = 100; gate = 400.
    const steps = [step(1, 100), step(5, 180), step(10, 380), step(20, 900)];
    expect(detectKnee(steps)).toBe(10);
  });
  it("breaks on error rate over 1%", () => {
    const steps = [step(1, 100), step(5, 120, 0.05)];
    expect(detectKnee(steps)).toBe(1); // step 5 fails the error gate
  });
  it("null when even baseline is dirty", () => {
    expect(detectKnee([step(1, 100, 0.2)])).toBeNull();
  });
  it("null on empty", () => {
    expect(detectKnee([])).toBeNull();
  });
});

describe("kneeConfirmed", () => {
  it("confirmed when a step above the knee ran (we witnessed degradation)", () => {
    const steps = [step(1, 100), step(5, 180), step(10, 380), step(20, 900)];
    expect(detectKnee(steps)).toBe(10);
    expect(kneeConfirmed(steps, 10)).toBe(true);
  });
  it("unconfirmed when the top step still passed (ramp ran out before the knee)", () => {
    const steps = [step(1, 100), step(5, 150), step(10, 200)];
    expect(detectKnee(steps)).toBe(10);
    expect(kneeConfirmed(steps, 10)).toBe(false); // never saw it break — ≥10, not "at 10"
  });
  it("false when there is no knee", () => {
    expect(kneeConfirmed([step(1, 100)], null)).toBe(false);
  });
});
