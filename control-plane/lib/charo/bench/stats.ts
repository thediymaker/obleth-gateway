import type { StepOutcome } from "./types";

export interface Sample { status: number; ttfbMs: number; totalMs: number; inTokens: number; outTokens: number }

/** Nearest-rank percentile — exact port of obench `engine::stats::percentile`. */
export function percentile(values: number[], p: number): number {
  if (values.length === 0) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  const idx = Math.floor((p / 100) * sorted.length);
  return sorted[Math.min(idx, sorted.length - 1)];
}

export function summarizeStep(concurrency: number, samples: Sample[], elapsedS: number): StepOutcome {
  let completed = 0, rejected = 0, errors = 0, inTok = 0, outTok = 0;
  const ttfb: number[] = [], total: number[] = [];
  for (const s of samples) {
    if (s.status === 200) {
      completed++; ttfb.push(s.ttfbMs); total.push(s.totalMs); inTok += s.inTokens; outTok += s.outTokens;
    } else if (s.status === 429) { rejected++; }
    else { errors++; }
  }
  const attempts = completed + rejected + errors;
  const errorRate = attempts > 0 ? errors / attempts : 0;
  const reqPerS = elapsedS > 0 ? completed / elapsedS : 0;
  const tokensPerS = elapsedS > 0 ? outTok / elapsedS : 0;
  return {
    concurrency, completed, rejected, errors, errorRate,
    p50TtfbMs: percentile(ttfb, 50), p90TtfbMs: percentile(ttfb, 90), p99TtfbMs: percentile(ttfb, 99),
    p50TotalMs: percentile(total, 50), p99TotalMs: percentile(total, 99),
    reqPerS, tokensPerS,
  };
}
