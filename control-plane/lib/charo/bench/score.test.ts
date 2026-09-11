import { describe, it, expect } from "vitest";
import { gradeFromScore, scoreBench } from "./score";
import { detectKnee } from "./knee";
import type { StepOutcome } from "./types";

const step = (
  c: number,
  p99: number,
  reqPerS: number,
  errors = 0,
  completed = 100,
  decodeTps = 0,
): StepOutcome => ({
  concurrency: c, completed, rejected: 0, errors, errorRate: errors / (completed + errors),
  p50TtfbMs: p99 / 2, p90TtfbMs: p99, p99TtfbMs: p99, p50TotalMs: p99, p99TotalMs: p99,
  reqPerS, tokensPerS: reqPerS * 10,
  p50DecodeTps: decodeTps, p10DecodeTps: decodeTps,
});

describe("gradeFromScore (obench thresholds)", () => {
  it("maps thresholds", () => {
    expect(gradeFromScore(90)).toBe("A");
    expect(gradeFromScore(75)).toBe("B");
    expect(gradeFromScore(60)).toBe("C");
    expect(gradeFromScore(45)).toBe("D");
    expect(gradeFromScore(44)).toBe("F");
  });
});

describe("scoreBench", () => {
  it("grades a ramp that never stopped scaling an A", () => {
    // Real 31B ramp: every step bought throughput, per-stream decode held at
    // the single-stream rate, no errors. TTFT rose 8x off the idle floor, which
    // is queueing, not a fault — it must not cost the model its grade.
    const steps = [
      step(1, 634, 0.5, 0, 100, 42),
      step(5, 3488, 1.2, 0, 100, 26),
      step(10, 3708, 2.5, 0, 100, 39),
      step(20, 3972, 4.8, 0, 100, 40),
      step(40, 5270, 6.8, 0, 100, 42),
    ];
    const { score, findings } = scoreBench(steps, detectKnee(steps));
    expect(score).toBe(100);
    expect(gradeFromScore(score)).toBe("A");
    expect(findings.some((f) => /not reached|healthy through/i.test(f))).toBe(true);
  });

  it("scores a model that never converts load into throughput near the floor", () => {
    // 5x the concurrency buys 2% more throughput: no batching at all.
    const steps = [step(1, 100, 1, 0, 100, 40), step(5, 400, 1.02, 0, 100, 40)];
    const { score, findings } = scoreBench(steps, detectKnee(steps));
    expect(score).toBe(50); // scaling 0/50, stability 30/30, cleanliness 20/20
    expect(findings.some((f) => /^Knee at concurrency 1\b/.test(f))).toBe(true);
  });

  it("charges per-stream decode collapse to the stability component", () => {
    // Throughput scales fine, but each caller's stream drops 40 -> 10 tok/s.
    const steps = [step(1, 100, 1, 0, 100, 40), step(5, 150, 5, 0, 100, 10)];
    const { score } = scoreBench(steps, detectKnee(steps));
    expect(score).toBe(78); // 50 scaling + 7.5 stability (25%) + 20 cleanliness
  });

  it("does not punish a non-streaming ramp for unmeasurable decode rate", () => {
    const steps = [step(1, 100, 1), step(5, 150, 5)];
    expect(scoreBench(steps, detectKnee(steps)).score).toBe(100);
  });

  it("scores an unsustainable model an F and says why", () => {
    const steps = [step(1, 100, 1, 50)]; // baseline already erroring
    const { score, findings } = scoreBench(steps, detectKnee(steps));
    expect(score).toBeLessThan(45);
    expect(findings.some((f) => /no knee/i.test(f))).toBe(true);
  });

  it("states a confirmed knee as a verdict, with the reason the ramp stopped", () => {
    const steps = [step(1, 100, 1), step(5, 150, 5), step(10, 200, 10), step(20, 400, 10.5)];
    const { findings } = scoreBench(steps, detectKnee(steps));
    expect(findings.some((f) => /^Knee at concurrency 10\b/.test(f))).toBe(true);
    expect(findings.some((f) => /throughput/i.test(f))).toBe(true);
  });

  it("states an unconfirmed knee as a floor, not a verdict", () => {
    const steps = [step(1, 100, 1), step(5, 150, 5), step(10, 200, 10)];
    const { findings } = scoreBench(steps, detectKnee(steps));
    expect(findings.some((f) => /not reached|healthy through/i.test(f))).toBe(true);
    expect(findings.some((f) => /^Knee at concurrency/.test(f))).toBe(false);
  });

  it("reports rejected requests as backpressure, not errors", () => {
    const steps = [step(1, 100, 1), { ...step(5, 150, 5), rejected: 9 }];
    const { findings } = scoreBench(steps, detectKnee(steps));
    expect(findings.some((f) => /429/.test(f))).toBe(true);
  });
});
