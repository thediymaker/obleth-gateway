import { registerTool } from "./registry";
import { runBenchmarkTool } from "@/lib/charo/bench/run-benchmark";

// Registered lazily & idempotently so route modules don't depend on import order.
let done = false;
export function ensureToolsRegistered(): void {
  if (done) return;
  done = true;
  registerTool(runBenchmarkTool);
}
