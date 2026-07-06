import { describe, it, expect } from "vitest";
import { detectKnee } from "./knee";
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
