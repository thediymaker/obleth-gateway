"use client";

import { Badge } from "@/components/ui/badge";
import {
  ResponsiveContainer, ComposedChart, Line, XAxis, YAxis, Tooltip, CartesianGrid, ReferenceLine, Legend,
} from "recharts";
import type { BenchResult } from "@/lib/charo/bench/types";

const GRADE_TONE: Record<string, string> = {
  A: "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400",
  B: "bg-emerald-500/10 text-emerald-600 dark:text-emerald-400",
  C: "bg-amber-500/15 text-amber-600 dark:text-amber-400",
  D: "bg-orange-500/15 text-orange-600 dark:text-orange-400",
  F: "bg-destructive/15 text-destructive",
};

export function BenchResultCard({ data }: { data: unknown }) {
  const r = data as Partial<BenchResult>;
  const steps = r.steps ?? [];
  const chart = steps.map((s) => ({
    concurrency: s.concurrency, p50: s.p50TtfbMs, p99: s.p99TtfbMs, req: Number(s.reqPerS.toFixed(2)),
  }));

  return (
    <div className="w-full space-y-3 rounded-lg border border-border bg-card p-3">
      <div className="flex items-center justify-between gap-2">
        <div className="min-w-0">
          <div className="truncate text-sm font-semibold">{r.modelName ?? "benchmark"}</div>
          <div className="text-xs text-muted-foreground">
            {r.kneeConcurrency != null ? `Knee at ${r.kneeConcurrency} concurrent` : "No knee detected"}
          </div>
        </div>
        {r.grade && (
          <Badge className={GRADE_TONE[r.grade] ?? ""}>
            {r.grade} · {r.score ?? 0}/100
          </Badge>
        )}
      </div>

      <div className="h-56 w-full">
        <ResponsiveContainer width="100%" height="100%">
          <ComposedChart data={chart} margin={{ top: 8, right: 8, bottom: 4, left: -8 }}>
            <CartesianGrid strokeDasharray="3 3" className="stroke-border/50" />
            <XAxis dataKey="concurrency" tick={{ fontSize: 11 }} label={{ value: "concurrency", position: "insideBottom", offset: -2, fontSize: 11 }} />
            <YAxis yAxisId="lat" tick={{ fontSize: 11 }} width={44} />
            <YAxis yAxisId="rps" orientation="right" tick={{ fontSize: 11 }} width={40} />
            <Tooltip contentStyle={{ fontSize: 12 }} />
            <Legend wrapperStyle={{ fontSize: 11 }} />
            {r.kneeConcurrency != null && (
              <ReferenceLine yAxisId="lat" x={r.kneeConcurrency} stroke="hsl(267 86% 66%)" strokeDasharray="4 3" label={{ value: "knee", fontSize: 10 }} />
            )}
            <Line yAxisId="lat" type="monotone" dataKey="p50" name="p50 TTFT ms" stroke="hsl(189 82% 45%)" dot={false} isAnimationActive={false} />
            <Line yAxisId="lat" type="monotone" dataKey="p99" name="p99 TTFT ms" stroke="hsl(267 86% 66%)" dot={false} isAnimationActive={false} />
            <Line yAxisId="rps" type="monotone" dataKey="req" name="req/s" stroke="hsl(142 71% 45%)" dot={false} isAnimationActive={false} />
          </ComposedChart>
        </ResponsiveContainer>
      </div>

      <div className="grid grid-cols-3 gap-2 text-xs">
        {steps.map((s) => (
          <div key={s.concurrency} className="rounded-md border border-border/60 bg-muted/30 p-2">
            <div className="font-medium">×{s.concurrency}</div>
            <div className="text-muted-foreground">{s.reqPerS.toFixed(1)} req/s</div>
            <div className="text-muted-foreground">p99 {s.p99TtfbMs}ms</div>
            {(s.errors > 0 || s.rejected > 0) && (
              <div className="text-muted-foreground">{s.errors} err · {s.rejected} rej</div>
            )}
          </div>
        ))}
      </div>

      {(r.findings?.length ?? 0) > 0 && (
        <ul className="list-disc space-y-0.5 pl-4 text-xs text-muted-foreground">
          {r.findings!.map((f, i) => <li key={i}>{f}</li>)}
        </ul>
      )}
    </div>
  );
}
