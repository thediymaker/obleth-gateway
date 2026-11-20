import type { GatewayChatBody } from "@/lib/charo/gateway";
import type { StepOutcome } from "./types";
import { summarizeStep, type Sample } from "./stats";
import { deltaText, readUpstreamSse } from "@/lib/charo/relay";

type Gateway = (body: GatewayChatBody, signal?: AbortSignal) => Promise<Response>;

/** Build a synthetic prompt near a target input-token count (~4 chars/token). */
export function synthPrompt(inputTokens: number): string {
  const words = Math.max(1, Math.round((inputTokens * 4) / 5));
  return Array.from({ length: words }, (_, i) => `w${i % 97}`).join(" ");
}

export async function runOneRequest(
  model: string, prompt: string, maxTokens: number, gateway: Gateway, signal: AbortSignal,
): Promise<Sample> {
  const start = Date.now();
  try {
    const res = await gateway(
      { model, messages: [{ role: "user", content: prompt }], stream: true, max_tokens: maxTokens },
      signal,
    );
    if (res.status === 429) { await res.body?.cancel().catch(() => {}); return { status: 429, ttfbMs: 0, totalMs: Date.now() - start, inTokens: 0, outTokens: 0 }; }
    if (!res.ok || !res.body) { await res.body?.cancel().catch(() => {}); return { status: res.status || 599, ttfbMs: 0, totalMs: Date.now() - start, inTokens: 0, outTokens: 0 }; }

    let ttfbMs = 0, outTokens = 0;
    await readUpstreamSse(res, (chunk) => {
      if (!deltaText(chunk)) return;
      if (ttfbMs === 0) ttfbMs = Date.now() - start;
      outTokens += 1; // coarse token proxy: one delta ≈ one token
    });
    return { status: 200, ttfbMs: ttfbMs || Date.now() - start, totalMs: Date.now() - start, inTokens: 0, outTokens };
  } catch (e) {
    if (signal.aborted) return { status: 499, ttfbMs: 0, totalMs: Date.now() - start, inTokens: 0, outTokens: 0 };
    return { status: 598, ttfbMs: 0, totalMs: Date.now() - start, inTokens: 0, outTokens: 0 };
  }
}

export interface RampCaps { maxConcurrency: number; maxDurationS: number; maxRequests: number }
export interface RampOpts {
  model: string; steps: number[]; stepDurationS: number; inputTokens: number; maxTokens: number;
  caps: RampCaps; gateway: Gateway; signal: AbortSignal; onStep?: (s: StepOutcome) => void;
}

export async function runRamp(opts: RampOpts): Promise<{ steps: StepOutcome[]; capped?: string }> {
  const prompt = synthPrompt(opts.inputTokens);
  const stepList = [...new Set(opts.steps)].sort((a, b) => a - b).filter((c) => c >= 1 && c <= opts.caps.maxConcurrency);
  const out: StepOutcome[] = [];
  let capped: string | undefined;
  let totalRequests = 0;
  const runStart = Date.now();

  for (const concurrency of stepList) {
    if (opts.signal.aborted) { capped = "aborted"; break; }
    if ((Date.now() - runStart) / 1000 >= opts.caps.maxDurationS) { capped = `duration cap (${opts.caps.maxDurationS}s)`; break; }
    if (totalRequests >= opts.caps.maxRequests) { capped = `request cap (${opts.caps.maxRequests})`; break; }

    const samples: Sample[] = [];
    const stepStart = Date.now();
    const stepEnd = stepStart + opts.stepDurationS * 1000;
    const worker = async () => {
      while (Date.now() < stepEnd && !opts.signal.aborted) {
        if (totalRequests >= opts.caps.maxRequests) break;
        totalRequests++;
        samples.push(await runOneRequest(opts.model, prompt, opts.maxTokens, opts.gateway, opts.signal));
      }
    };
    await Promise.all(Array.from({ length: concurrency }, worker));
    const elapsedS = (Date.now() - stepStart) / 1000;
    const summary = summarizeStep(concurrency, samples, elapsedS);
    out.push(summary);
    opts.onStep?.(summary);

    if (totalRequests >= opts.caps.maxRequests) { capped = `request cap (${opts.caps.maxRequests})`; break; }
  }
  return { steps: out, capped };
}
