"use client";

import type { TraceSummary } from "@/lib/charo/trace";
import { formatDurationMs } from "@/lib/format";
import { formatCurrency } from "@/lib/utils";
import Link from "next/link";
import type { ChatTurn } from "./use-charo-stream";

const BOON_LABELS: Record<string, string> = {
  vision: "vision",
  tool_loop: "tools / search",
  guardrails_input: "guardrails (in)",
  guardrails_output: "guardrails (out)",
  structured_repair: "structured output",
};

function label(b: string): string {
  return BOON_LABELS[b] ?? b;
}


export function TraceCard({
  trace,
  pending,
  configured = [],
  requestId,
  metrics,
}: {
  trace: TraceSummary | null | undefined;
  pending?: boolean;
  configured?: string[];
  requestId?: string;
  metrics?: ChatTurn["metrics"];
}) {
  if (!trace) {
    return (
      <p className="font-mono text-[10.5px] text-muted-foreground/70">
        {pending ? "trace pending — telemetry still flushing…" : "no trace available"}
        {metrics && <span className="block">{metrics.outputTokens ?? "—"} output tokens · TTFT {metrics.ttftMs === undefined ? "—" : formatDurationMs(metrics.ttftMs)} · {formatDurationMs(metrics.totalMs)} total</span>}
        {requestId && <Link className="ml-2 underline" href={`/logs?requestId=${encodeURIComponent(requestId)}`}>Inspect request</Link>}
      </p>
    );
  }

  const fired = new Set(trace.boonsFired);
  const configuredNorm = configured.map((c) => (c === "structured_output" ? "structured_repair" : c));
  const notFired = configuredNorm.filter((c) => !fired.has(c));
  const isError = trace.statusCode >= 400 || trace.errorStages.length > 0;

  const stats = [
    `tok ${metrics?.inputTokens ?? trace.inputTokens}/${metrics?.outputTokens ?? trace.outputTokens}`,
    `ttft ${formatDurationMs(metrics?.ttftMs ?? trace.ttftMs)}`,
    `total ${formatDurationMs(metrics?.totalMs ?? trace.totalMs)}`,
    `cache ${trace.cacheStatus}`,
    trace.accountingAvailable === false ? "cost unavailable" : formatCurrency(trace.costUsd),
    ...(trace.energyWh === undefined ? [] : [`${trace.energyWh.toFixed(4)} Wh`]),
    ...(isError ? [`http ${trace.statusCode || "—"}`] : []),
  ];

  return (
    <div className="space-y-0.5 font-mono text-[10.5px] leading-relaxed text-muted-foreground/70">
      {requestId && <Link className="inline-block text-foreground underline" href={`/logs?requestId=${encodeURIComponent(requestId)}`}>Inspect request</Link>}
      <div className="flex flex-wrap gap-x-1.5">
        {trace.boonsFired.length === 0 && notFired.length === 0 && <span>boons none</span>}
        {trace.boonsFired.map((b) => (
          <span key={b} className="text-emerald-600 dark:text-emerald-400/90">✓ {label(b)}</span>
        ))}
        {notFired.map((b) => (
          <span key={b} className="text-muted-foreground/50">{label(b)}</span>
        ))}
        {trace.toolLoopIters > 0 && (
          <span>
            · {trace.toolLoopIters} tool turn{trace.toolLoopIters === 1 ? "" : "s"}: {trace.toolsCalled.join(", ")}
          </span>
        )}
        <span>· {stats.join(" · ")}</span>
      </div>
      {trace.errorStages.length > 0 && (
        <div className="text-destructive">failed at: {trace.errorStages.join(", ")}</div>
      )}
    </div>
  );
}
