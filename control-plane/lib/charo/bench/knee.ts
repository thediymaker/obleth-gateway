import type { StepOutcome } from "./types";

const MAX_ERROR_RATE = 0.01;
/** p99 TTFT growth vs the *previous* step that counts as "latency climbing". */
const LATENCY_GROWTH = 1.5;
/** Throughput gain vs the previous step below which more load buys nothing. */
const THROUGHPUT_FLOOR_GAIN = 1.1;

export interface KneeVerdict {
  /** Highest concurrency the model sustained; null if it never held one. */
  concurrency: number | null;
  /** True only when a step above the knee actually degraded (see below). */
  confirmed: boolean;
  /** Why the ramp stopped; null when it finished without degrading. */
  reason: string | null;
}

/**
 * Saturation is a *step-over-step* judgement: the knee is where added
 * concurrency stops buying throughput, so a step only ends the ramp when its
 * tail latency climbs AND its throughput flattens against the step below it.
 *
 * Gating on an absolute multiple of the concurrency-1 p99 instead — the rule
 * this replaced — mistakes batching for saturation. TTFT at concurrency 1 is
 * the idle floor, with nothing queued ahead of you; the moment you batch, every
 * request waits behind others in prefill, so TTFT jumps well past 4x the floor
 * on the first concurrent step of a perfectly healthy model. That gate reported
 * a 31B model that scaled cleanly to 40 concurrent as "knee at 1".
 *
 * Mirrors `evaluate()` in obench (`src/engine/calibrate.rs`), which is the
 * reference implementation for this rule.
 */
export function detectKnee(steps: StepOutcome[]): KneeVerdict {
  const ordered = [...steps].sort((a, b) => a.concurrency - b.concurrency);
  let lastClean: StepOutcome | null = null;

  for (const s of ordered) {
    const reason = stopReason(lastClean, s);
    if (reason) {
      return { concurrency: lastClean?.concurrency ?? null, confirmed: lastClean !== null, reason };
    }
    lastClean = s;
  }

  // The ramp ran out before the model did, so the top step is a floor ("we know
  // it holds >= this"), not a ceiling. Reporting it as a verdict is what makes a
  // 32-replica model look like it "tops out at 10".
  return { concurrency: lastClean?.concurrency ?? null, confirmed: false, reason: null };
}

function stopReason(prev: StepOutcome | null, s: StepOutcome): string | null {
  if (s.errorRate > MAX_ERROR_RATE) {
    return `error rate ${(s.errorRate * 100).toFixed(1)}% at ${s.concurrency} concurrent crossed the 1% ceiling`;
  }
  if (!prev || prev.p99TtfbMs <= 0 || prev.reqPerS <= 0) return null;

  const latencyClimb = s.p99TtfbMs >= prev.p99TtfbMs * LATENCY_GROWTH;
  const throughputFlat = s.reqPerS < prev.reqPerS * THROUGHPUT_FLOOR_GAIN;
  if (!latencyClimb || !throughputFlat) return null;

  const gainPct = (s.reqPerS / prev.reqPerS - 1) * 100;
  return (
    `at ${s.concurrency} concurrent p99 TTFT climbed ${(s.p99TtfbMs / prev.p99TtfbMs).toFixed(1)}x ` +
    `while throughput gained only ${gainPct.toFixed(0)}%`
  );
}
