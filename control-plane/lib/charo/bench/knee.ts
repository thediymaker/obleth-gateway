import type { StepOutcome } from "./types";

const MAX_ERROR_RATE = 0.01;
const LATENCY_GATE_MULT = 4;

export function detectKnee(steps: StepOutcome[]): number | null {
  if (steps.length === 0) return null;
  const baseline = steps.reduce((a, b) => (b.concurrency < a.concurrency ? b : a), steps[0]);
  const gate = baseline.p99TtfbMs * LATENCY_GATE_MULT;
  let knee: number | null = null;
  for (const s of steps) {
    const passes = s.errorRate <= MAX_ERROR_RATE && (gate === 0 ? s.p99TtfbMs === 0 : s.p99TtfbMs <= gate);
    if (passes && (knee === null || s.concurrency > knee)) knee = s.concurrency;
  }
  return knee;
}
