import type { StepOutcome, BenchResult } from "./types";
import type { KneeVerdict } from "./knee";

export function gradeFromScore(score: number): BenchResult["grade"] {
  if (score >= 90) return "A";
  if (score >= 75) return "B";
  if (score >= 60) return "C";
  if (score >= 45) return "D";
  return "F";
}

const clamp = (n: number) => Math.max(0, Math.min(100, n));

/** Throughput gain over the step below that still counts as "load bought something". */
const USEFUL_GAIN = 1.1;

/**
 * The score grades *behaviour under load*, not absolute speed. Throughput in
 * req/s is a property of the hardware as much as the model, so grading it would
 * just re-rank the fleet by GPU — obench takes the same position for its
 * capacity section. What is gradeable is whether the model converted added
 * concurrency into throughput, held per-caller quality while doing it, and
 * stayed clean.
 *
 * The three axes are deliberately independent. An earlier version scored
 * throughput-at-the-knee against peak *and* p99 inflation against the
 * concurrency-1 baseline, which double-counts: by Little's law those are the
 * same fact measured twice, and when the knee collapsed onto the baseline step
 * they cancelled (throughput ~0, latency a free 100) and pinned every such run
 * at ~54/D regardless of how the model actually behaved.
 */
export function scoreBench(steps: StepOutcome[], knee: KneeVerdict): { score: number; findings: string[] } {
  const findings: string[] = [];
  if (steps.length === 0) return { score: 0, findings: ["No steps ran."] };

  const ordered = [...steps].sort((a, b) => a.concurrency - b.concurrency);
  const baseline = ordered[0];
  const kneeStep = knee.concurrency == null
    ? null
    : ordered.find((s) => s.concurrency === knee.concurrency) ?? null;

  // Scaling: of the steps the model actually sustained, how many bought real
  // throughput over the one below? A model that batches well gains on every
  // transition; one that serialises gains nothing on any of them.
  const sustained = kneeStep ? ordered.filter((s) => s.concurrency <= kneeStep.concurrency) : [];
  const transitions = sustained.slice(1);
  const useful = transitions.filter((s, i) => s.reqPerS >= sustained[i].reqPerS * USEFUL_GAIN);
  const scaling = transitions.length > 0 ? (useful.length / transitions.length) * 100 : 0;

  // Stability: does an individual caller's stream still decode at the speed it
  // did when the model was idle? This is what a user feels under load, and it
  // is independent of queueing. Non-streaming ramps can't measure it, so they
  // are not charged for it.
  const decodeMeasurable = baseline.p50DecodeTps > 0 && (kneeStep?.p50DecodeTps ?? 0) >= 0;
  const stability = !kneeStep
    ? 0
    : decodeMeasurable
    ? clamp((kneeStep.p50DecodeTps / baseline.p50DecodeTps) * 100)
    : 100;

  const totalErrors = ordered.reduce((a, s) => a + s.errors, 0);
  const totalAttempts = ordered.reduce((a, s) => a + s.completed + s.errors, 0); // 429s excluded
  const cleanliness = totalAttempts > 0 ? clamp((1 - totalErrors / totalAttempts) * 100) : 100;

  const score = Math.round(clamp(0.5 * scaling + 0.3 * stability + 0.2 * cleanliness));

  if (kneeStep && knee.confirmed) {
    findings.push(
      `Knee at concurrency ${kneeStep.concurrency} (${kneeStep.reqPerS.toFixed(1)} req/s)` +
        `${knee.reason ? ` — ${knee.reason}` : ""}.`,
    );
  } else if (kneeStep) {
    findings.push(
      `Knee not reached — healthy through ${kneeStep.concurrency} concurrent ` +
        `(${kneeStep.reqPerS.toFixed(1)} req/s). Raise the concurrency/request caps or add higher steps to find the real ceiling.`,
    );
  } else {
    findings.push(
      `No knee: the model did not sustain even the lowest step` +
        `${knee.reason ? ` — ${knee.reason}` : ""}.`,
    );
  }

  if (kneeStep && transitions.length > 0 && useful.length < transitions.length) {
    const flat = transitions.filter((s) => !useful.includes(s)).map((s) => s.concurrency);
    findings.push(`Added concurrency bought little throughput at ${flat.join(", ")} — below ${kneeStep.concurrency} is the efficient range.`);
  }
  if (kneeStep && decodeMeasurable && stability < 80) {
    findings.push(
      `Per-stream decode fell from ${baseline.p50DecodeTps.toFixed(0)} to ` +
        `${kneeStep.p50DecodeTps.toFixed(0)} tok/s at the knee (${stability.toFixed(0)}/100 stability).`,
    );
  }
  if (totalErrors > 0) findings.push(`${totalErrors} errored requests across the ramp (${cleanliness.toFixed(0)}/100 cleanliness).`);
  if (ordered.some((s) => s.rejected > 0)) findings.push("Some requests were rejected (429) — healthy backpressure, not counted as errors.");

  return { score, findings };
}
