"use client";

import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";
import type { TraceSummary } from "@/lib/charo/trace";

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

function ms(n: number): string {
  return n >= 1000 ? `${(n / 1000).toFixed(2)}s` : `${Math.round(n)}ms`;
}

export function TraceCard({
  trace,
  pending,
  configured = [],
}: {
  trace: TraceSummary | null | undefined;
  pending?: boolean;
  configured?: string[];
}) {
  if (!trace) {
    return (
      <div className="mt-2 rounded-md border border-border/60 bg-muted/30 px-3 py-2 text-xs text-muted-foreground">
        {pending ? "Trace pending — telemetry still flushing…" : "No trace available."}
      </div>
    );
  }

  const fired = new Set(trace.boonsFired);
  // Configured-but-not-fired chips (greyed). Map the model's boon catalog names
  // onto the trace's vocabulary where they differ.
  const configuredNorm = configured.map((c) =>
    c === "structured_output" ? "structured_repair" : c,
  );
  const notFired = configuredNorm.filter((c) => !fired.has(c));

  const isError = trace.statusCode >= 400 || trace.errorStages.length > 0;

  return (
    <div
      className={cn(
        "mt-2 space-y-2 rounded-md border px-3 py-2 text-xs",
        isError
          ? "border-destructive/40 bg-destructive/5"
          : "border-border/60 bg-muted/30",
      )}
    >
      <div className="flex flex-wrap items-center gap-1.5">
        <span className="text-muted-foreground">boons:</span>
        {trace.boonsFired.length === 0 && notFired.length === 0 && (
          <span className="text-muted-foreground">none fired</span>
        )}
        {trace.boonsFired.map((b) => (
          <Badge
            key={b}
            className="border-emerald-500/40 bg-emerald-500/10 text-emerald-600 dark:text-emerald-400"
          >
            ✓ {label(b)}
          </Badge>
        ))}
        {notFired.map((b) => (
          <Badge key={b} className="opacity-60">
            {label(b)}
          </Badge>
        ))}
      </div>

      {trace.toolLoopIters > 0 && (
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="text-muted-foreground">
            {trace.toolLoopIters} tool turn{trace.toolLoopIters === 1 ? "" : "s"}:
          </span>
          {trace.toolsCalled.map((t, i) => (
            <Badge key={`${t}-${i}`} className="font-mono">
              {t}
            </Badge>
          ))}
        </div>
      )}

      {trace.errorStages.length > 0 && (
        <div className="text-destructive">
          failed at: {trace.errorStages.join(", ")}
        </div>
      )}

      <div className="flex flex-wrap gap-x-4 gap-y-1 text-muted-foreground">
        <span>
          tok {trace.inputTokens}/{trace.outputTokens}
        </span>
        <span>ttft {ms(trace.ttftMs)}</span>
        <span>total {ms(trace.totalMs)}</span>
        <span>cache {trace.cacheStatus}</span>
        <span>${trace.costUsd.toFixed(5)}</span>
        <span className={cn(isError && "text-destructive")}>
          http {trace.statusCode || "—"}
        </span>
      </div>
    </div>
  );
}
