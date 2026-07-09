export interface StepOutcome {
  concurrency: number;
  completed: number; rejected: number; errors: number; errorRate: number;
  p50TtfbMs: number; p90TtfbMs: number; p99TtfbMs: number;
  p50TotalMs: number; p99TotalMs: number;
  reqPerS: number; tokensPerS: number;
  p50DecodeTps: number; p10DecodeTps: number;
}
export interface BenchResult {
  modelId: string;             // model UUID — future persistence key
  modelName: string;
  configFingerprint: string;
  startedAt: string;           // ISO
  steps: StepOutcome[];
  kneeConcurrency: number | null;
  kneeConfirmed: boolean;      // true only if a higher step degraded above the knee (else knee is a floor, "≥")
  score: number;               // 0–100
  grade: "A" | "B" | "C" | "D" | "F";
  findings: string[];
  capped?: string;             // set when a cap stopped the run early
}
