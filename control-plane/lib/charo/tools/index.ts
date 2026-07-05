import { registerTool, getTool } from "./registry";

// Registered lazily & idempotently so route modules don't depend on import order.
let done = false;
export function ensureToolsRegistered(): void {
  if (done) return;
  done = true;
  // Built-in tools register here. run_benchmark is added in Phase C:
  // import { runBenchmarkTool } from "./run-benchmark"; registerTool(runBenchmarkTool);
  void registerTool; void getTool;
}
