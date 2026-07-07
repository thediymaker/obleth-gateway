import type { ComponentType } from "react";
import { BenchResultCard } from "./bench-result-card";
import { CapabilityResultCard } from "./capability-result-card";

function JsonFallback({ data }: { data: unknown }) {
  return (
    <pre className="max-h-64 overflow-auto rounded-md border border-border bg-muted/40 p-2 text-xs">
      {JSON.stringify(data, null, 2)}
    </pre>
  );
}

const REGISTRY: Record<string, ComponentType<{ data: unknown }>> = {
  bench_result: BenchResultCard as ComponentType<{ data: unknown }>,
  capability_result: CapabilityResultCard as ComponentType<{ data: unknown }>,
  tool_error: ({ data }) => (
    <p className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
      {String((data as { message?: string })?.message ?? "tool error")}
    </p>
  ),
};

export function resultRenderer(type: string): ComponentType<{ data: unknown }> {
  return REGISTRY[type] ?? JsonFallback;
}
