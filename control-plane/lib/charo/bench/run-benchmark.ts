import type { CharoTool, ToolCtx, ToolProgress } from "@/lib/charo/tools/types";
import type { BenchResult } from "./types";
import { runRamp } from "./ramp";
import { detectKnee } from "./knee";
import { scoreBench, gradeFromScore } from "./score";
import { configFingerprint } from "./fingerprint";
import { obleth } from "@/lib/obleth";

export interface BenchArgs {
  model: string; steps: number[]; stepDurationS: number; inputTokens: number; maxTokens: number;
}

const DEFAULT_STEPS = [1, 5, 10, 20, 40];

export const runBenchmarkTool: CharoTool<BenchArgs, BenchResult> = {
  name: "run_benchmark",
  description:
    "Run a concurrency-ramp capacity benchmark against a model served by this gateway. " +
    "Returns per-step latency/throughput, a detected capacity knee, and a 0–100 score with an A–F grade.",
  parameters: {
    type: "object",
    properties: {
      model: { type: "string", description: "Model name (as configured in the gateway) to benchmark." },
      steps: { type: "array", items: { type: "number" }, description: "Concurrency levels to ramp through." },
      step_duration_s: { type: "number", description: "Seconds to hold each concurrency step." },
      input_tokens: { type: "number", description: "Approximate synthetic prompt size." },
      max_tokens: { type: "number", description: "Output tokens to request per call." },
    },
    required: ["model"],
    additionalProperties: false,
  },
  resultType: "bench_result",
  requiresConfirmation: true,

  parseArgs(raw: unknown): BenchArgs {
    const o = (raw ?? {}) as Record<string, unknown>;
    const model = typeof o.model === "string" ? o.model.trim() : "";
    if (!model) throw new Error("run_benchmark requires a `model`.");
    const steps = Array.isArray(o.steps) && o.steps.length
      ? (o.steps as unknown[]).map(Number).filter((n) => Number.isFinite(n) && n >= 1)
      : DEFAULT_STEPS;
    return {
      model,
      steps,
      stepDurationS: Number.isFinite(Number(o.step_duration_s)) ? Number(o.step_duration_s) : 10,
      inputTokens: Number.isFinite(Number(o.input_tokens)) ? Number(o.input_tokens) : 256,
      maxTokens: Number.isFinite(Number(o.max_tokens)) ? Number(o.max_tokens) : 64,
    };
  },

  async run(args: BenchArgs, ctx: ToolCtx, emit: (p: ToolProgress) => void): Promise<BenchResult> {
    const models = await obleth.listModels().catch(() => []);
    const model = models.find((m) => m.model_name === args.model);
    if (!model) throw new Error(`model not found: ${args.model}`);
    const endpoints = await obleth.listModelEndpoints(model.id).catch(() => []);

    const caps = {
      maxConcurrency: ctx.settings.bench_max_concurrency,
      maxDurationS: ctx.settings.bench_max_duration_s,
      maxRequests: ctx.settings.bench_max_requests,
    };

    emit({ kind: "bench_start", model: args.model, steps: args.steps });
    const { steps, capped } = await runRamp({
      model: args.model, steps: args.steps, stepDurationS: args.stepDurationS,
      inputTokens: args.inputTokens, maxTokens: args.maxTokens, caps,
      gateway: ctx.gatewayChat, signal: ctx.signal,
      onStep: (s) => emit({ kind: "bench_step", step: s }),
    });

    const kneeConcurrency = detectKnee(steps);
    const { score, findings } = scoreBench(steps, kneeConcurrency);
    if (capped) findings.push(`Run stopped early: ${capped}.`);

    return {
      modelId: model.id,
      modelName: model.model_name,
      configFingerprint: configFingerprint(model, endpoints),
      startedAt: new Date().toISOString(),
      steps, kneeConcurrency, score, grade: gradeFromScore(score), findings,
      ...(capped ? { capped } : {}),
    };
  },
};
