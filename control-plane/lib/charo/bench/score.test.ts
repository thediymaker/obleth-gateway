import { describe, it, expect } from "vitest";
import { gradeFromScore, scoreBench } from "./score";
import type { StepOutcome } from "./types";

const step = (c: number, p99: number, reqPerS: number, errors = 0, completed = 100): StepOutcome => ({
  concurrency: c, completed, rejected: 0, errors, errorRate: errors / (completed + errors),
  p50TtfbMs: p99 / 2, p90TtfbMs: p99, p99TtfbMs: p99, p50TotalMs: p99, p99TotalMs: p99,
  reqPerS, tokensPerS: reqPerS * 10,
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
  it("clean linear ramp, knee at peak → high score, no adverse findings", () => {
    const steps = [step(1, 100, 1), step(5, 150, 5), step(10, 200, 10)];
    const { score, findings } = scoreBench(steps, 10);
    // throughput 10/10=100; latency ratio 2 → 100*(2/3)=66.7; cleanliness 100
    // 0.5*100 + 0.3*66.7 + 0.2*100 = 90.0 → 90
    expect(score).toBe(90);
    expect(findings.some((f) => f.toLowerCase().includes("knee"))).toBe(true);
  });

  it("no knee → throughput+latency zero, finding explains", () => {
    const steps = [step(1, 100, 1, 50)]; // baseline dirty
    const { score, findings } = scoreBench(steps, null);
    expect(score).toBeLessThan(45);
    expect(findings.some((f) => f.toLowerCase().includes("no knee"))).toBe(true);
  });
});
