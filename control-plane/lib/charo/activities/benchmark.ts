import { Gauge } from "lucide-react";
import type { Activity } from "./types";

export const benchmarkActivity: Activity = {
  id: "benchmark",
  label: "Benchmark a model",
  blurb: "Concurrency ramp → score + grade",
  icon: Gauge,
  kind: "run",
  toolName: "run_benchmark",
  resultType: "bench_result",
  steps: [
    { type: "model", label: "Model" },
    { type: "number", key: "step_duration_s", label: "Seconds per step", default: 5, min: 1, max: 30 },
  ],
};
