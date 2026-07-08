import type { ComponentType } from "react";
import { BenchResultCard } from "./bench-result-card";
import { CapabilityResultCard } from "./capability-result-card";
import { McpTestCard } from "./mcp-test-card";
import { DocsResultCard } from "./docs-result-card";

function JsonFallback({ data }: { data: unknown }) {
  return (
    <pre className="max-h-64 overflow-auto rounded-md bg-white/[0.025] px-2.5 py-2 font-mono text-[11px] leading-normal text-muted-foreground">
      {JSON.stringify(data, null, 2)}
    </pre>
  );
}

const REGISTRY: Record<string, ComponentType<{ data: unknown }>> = {
  bench_result: BenchResultCard as ComponentType<{ data: unknown }>,
  capability_result: CapabilityResultCard as ComponentType<{ data: unknown }>,
  mcp_test_result: McpTestCard as ComponentType<{ data: unknown }>,
  docs_result: DocsResultCard as ComponentType<{ data: unknown }>,
  tool_error: ({ data }) => (
    <p className="border-l-2 border-destructive/50 pl-3 text-[13px] text-destructive">
      {String((data as { message?: string })?.message ?? "tool error")}
    </p>
  ),
};

export function resultRenderer(type: string): ComponentType<{ data: unknown }> {
  return REGISTRY[type] ?? JsonFallback;
}
