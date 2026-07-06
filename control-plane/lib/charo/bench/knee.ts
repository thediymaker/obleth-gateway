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

/**
 * A knee is only *confirmed* if we actually witnessed the model degrade above it —
 * i.e. some step ran at a higher concurrency (and, being above the knee, necessarily
 * failed the gate). If the top step that ran still passed, the ramp stopped short of
 * the real knee (steps exhausted or a cap hit), so the true capacity is `>= knee`,
 * not `= knee`. Reporting an unconfirmed knee as a verdict is what makes a 32-replica
 * model look like it "tops out at 10".
 */
export function kneeConfirmed(steps: StepOutcome[], knee: number | null): boolean {
  return knee !== null && steps.some((s) => s.concurrency > knee);
}
