import type { StepOutcome, BenchResult } from "./types";

export function gradeFromScore(score: number): BenchResult["grade"] {
  if (score >= 90) return "A";
  if (score >= 75) return "B";
  if (score >= 60) return "C";
  if (score >= 45) return "D";
  return "F";
}

const clamp = (n: number) => Math.max(0, Math.min(100, n));

export function scoreBench(steps: StepOutcome[], kneeConcurrency: number | null): { score: number; findings: string[] } {
  const findings: string[] = [];
  if (steps.length === 0) return { score: 0, findings: ["No steps ran."] };

  const baseline = steps.reduce((a, b) => (b.concurrency < a.concurrency ? b : a), steps[0]);
  const peakReqPerS = Math.max(...steps.map((s) => s.reqPerS));
  const kneeStep = kneeConcurrency == null ? null : steps.find((s) => s.concurrency === kneeConcurrency) ?? null;

  const throughput = kneeStep && peakReqPerS > 0 ? clamp((kneeStep.reqPerS / peakReqPerS) * 100) : 0;

  let latency = 0;
  if (kneeStep) {
    const ratio = baseline.p99TtfbMs > 0 ? kneeStep.p99TtfbMs / baseline.p99TtfbMs : 1;
    latency = clamp(ratio <= 1 ? 100 : ratio >= 4 ? 0 : (100 * (4 - ratio)) / 3);
  }

  const totalErrors = steps.reduce((a, s) => a + s.errors, 0);
  const totalAttempts = steps.reduce((a, s) => a + s.completed + s.errors, 0); // 429s excluded
  const cleanliness = totalAttempts > 0 ? clamp((1 - totalErrors / totalAttempts) * 100) : 100;

  const score = Math.round(clamp(0.5 * throughput + 0.3 * latency + 0.2 * cleanliness));

  if (kneeStep) findings.push(`Knee at concurrency ${kneeStep.concurrency} (${kneeStep.reqPerS.toFixed(1)} req/s).`);
  else findings.push("No knee found: no step held error rate ≤ 1% and p99 TTFT within 4× baseline.");
  if (totalErrors > 0) findings.push(`${totalErrors} errored requests across the ramp (${(cleanliness).toFixed(0)}/100 cleanliness).`);
  const anyRejected = steps.some((s) => s.rejected > 0);
  if (anyRejected) findings.push("Some requests were rejected (429) — healthy backpressure, not counted as errors.");

  return { score, findings };
}
