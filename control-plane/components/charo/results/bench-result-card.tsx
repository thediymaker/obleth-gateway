"use client";

import type { ReactNode } from "react";
import { Badge } from "@/components/ui/badge";
import {
  ResponsiveContainer, LineChart, Line, XAxis, YAxis, Tooltip, CartesianGrid, ReferenceLine,
} from "recharts";
import type { BenchResult } from "@/lib/charo/bench/types";
import { Rail, MicroLabel } from "@/components/charo/rail";
import { axisTick, chartGrid, timeCursor, tip } from "@/components/chart-tooltip";

const GRADE_TONE: Record<string, string> = {
  A: "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400",
  B: "bg-emerald-500/10 text-emerald-600 dark:text-emerald-400",
  C: "bg-amber-500/15 text-amber-600 dark:text-amber-400",
  D: "bg-orange-500/15 text-orange-600 dark:text-orange-400",
  F: "bg-destructive/15 text-destructive",
};

// Validated against the dark card surface (#111113): the three panel leads clear
// every all-pairs gate, and p99/p50 are one hue at two steps because they are the
// same measure, not two categories. Don't re-tune these by eye.
const P99 = "#3987e5";
const P50 = "#184f95";
const REQ = "#199e70";
const TOK = "#d95926";
const KNEE_LINE = "hsl(240 5% 38%)";

const ms = (v: number) => `${Math.round(v)} ms`;
const rps = (v: number) => `${v.toFixed(2)} req/s`;
const tps = (v: number) => `${Math.round(v)} tok/s`;
const atConcurrency = (label: string | number | undefined) => `${label} concurrent`;

/**
 * One measure per panel, sharing the concurrency axis. These three series are on
 * wildly different scales (ms, single-digit req/s, hundreds of tok/s) — overlaying
 * them on stacked y-axes made the crossings look meaningful when they were an
 * artefact of the scaling, and cost the plot three tick columns of chrome.
 */
function Panel({
  title, legend, height = "h-20", children,
}: {
  title: string;
  legend?: ReactNode;
  height?: string;
  children: ReactNode;
}) {
  return (
    <div>
      <div className="flex items-baseline justify-between gap-2">
        <MicroLabel>{title}</MicroLabel>
        {legend}
      </div>
      <div className={`w-full min-h-0 ${height}`}>{children}</div>
    </div>
  );
}

function SeriesKey({ color, label }: { color: string; label: string }) {
  return (
    <span className="flex items-center gap-1.5 text-[10.5px] text-muted-foreground">
      <span className="h-1.5 w-3 rounded-full" style={{ background: color }} />
      {label}
    </span>
  );
}

export function BenchResultCard({ data }: { data: unknown }) {
  const r = data as Partial<BenchResult>;
  const steps = r.steps ?? [];
  const chart = steps.map((s) => ({
    concurrency: s.concurrency, p50: s.p50TtfbMs, p99: s.p99TtfbMs,
    req: Number(s.reqPerS.toFixed(2)), tok: Math.round(s.tokensPerS ?? 0),
  }));

  // A knee is only a verdict if we witnessed the model degrade above it. Otherwise the
  // ramp stopped short (caps/steps), so the number is a floor ("≥"), not a ceiling.
  const kneeReached = r.kneeConcurrency != null && r.kneeConfirmed === true;
  const kneeFloor = r.kneeConcurrency != null && !kneeReached;
  const headline = kneeReached
    ? `Knee at ${r.kneeConcurrency} concurrent`
    : kneeFloor
    ? `Healthy through ${r.kneeConcurrency} concurrent · knee not reached`
    : "No knee detected";
  const singleStream = steps.length > 0 && steps[0].concurrency === 1 ? (steps[0].p50DecodeTps ?? 0) : 0;

  // Only a *confirmed* knee earns a marker. On an unconfirmed ramp the line would
  // sit on the last data point — no information, and it collides with the series
  // it's drawn over. "knee not reached" is already in the headline.
  const kneeMark = (label: boolean) =>
    kneeReached && r.kneeConcurrency != null ? (
      <ReferenceLine
        x={r.kneeConcurrency}
        stroke={KNEE_LINE}
        strokeDasharray="4 3"
        label={label ? { value: "knee", fontSize: 10, fill: "hsl(240 5% 58%)", position: "top" } : undefined}
      />
    ) : null;

  // The axis caption lives outside the SVG — recharts clips an `insideBottom`
  // label against the plot box at these panel heights.
  const xAxis = (show: boolean) => (
    <XAxis
      dataKey="concurrency"
      hide={!show}
      tick={axisTick}
      tickLine={false}
      axisLine={false}
      height={show ? 20 : 0}
    />
  );

  const yAxis = (fmt?: (v: number) => string) => (
    <YAxis
      tick={axisTick}
      tickLine={false}
      axisLine={false}
      width={46}
      tickFormatter={fmt}
      allowDecimals={false}
    />
  );

  return (
    <Rail className="w-full space-y-3">
      <div className="flex items-baseline justify-between gap-2">
        <div className="min-w-0">
          <div className="flex items-baseline gap-2">
            <span className="truncate text-[13px] font-semibold text-foreground">{r.modelName ?? "benchmark"}</span>
            <MicroLabel className="shrink-0">Benchmark</MicroLabel>
          </div>
          <div className="text-[11.5px] text-muted-foreground">
            {headline}
            {singleStream > 0 && ` · single-stream ${singleStream.toFixed(0)} tok/s`}
          </div>
        </div>
        {r.grade && (
          <div className="flex shrink-0 flex-col items-end gap-0.5">
            <Badge className={GRADE_TONE[r.grade] ?? ""}>{r.grade} · {r.score ?? 0}/100</Badge>
            {kneeFloor && (
              <span className="text-[10px] text-muted-foreground">
                {r.capped ? "provisional — ramp capped" : "no degradation seen"}
              </span>
            )}
          </div>
        )}
      </div>

      <div className="space-y-2">
        <Panel
          title="Time to first token"
          legend={
            <span className="flex items-center gap-3">
              <SeriesKey color={P99} label="p99" />
              <SeriesKey color={P50} label="p50" />
            </span>
          }
          height="h-24"
        >
          <ResponsiveContainer width="100%" height="100%">
            {/* extra top margin so the "knee" marker label isn't clipped */}
            <LineChart data={chart} margin={{ top: 16, right: 8, bottom: 0, left: -8 }}>
              <CartesianGrid {...chartGrid} vertical={false} />
              {xAxis(false)}
              {yAxis((v) => `${v}`)}
              <Tooltip cursor={timeCursor} content={tip({ valueFormatter: ms, labelFormatter: atConcurrency })} />
              {kneeMark(true)}
              <Line type="monotone" dataKey="p50" name="p50 TTFT" stroke={P50} strokeWidth={2} dot={false} activeDot={{ r: 3, strokeWidth: 0 }} isAnimationActive={false} />
              <Line type="monotone" dataKey="p99" name="p99 TTFT" stroke={P99} strokeWidth={2} dot={false} activeDot={{ r: 3, strokeWidth: 0 }} isAnimationActive={false} />
            </LineChart>
          </ResponsiveContainer>
        </Panel>

        <Panel title="Throughput · req/s">
          <ResponsiveContainer width="100%" height="100%">
            <LineChart data={chart} margin={{ top: 6, right: 8, bottom: 0, left: -8 }}>
              <CartesianGrid {...chartGrid} vertical={false} />
              {xAxis(false)}
              {yAxis()}
              <Tooltip cursor={timeCursor} content={tip({ valueFormatter: rps, labelFormatter: atConcurrency })} />
              {kneeMark(false)}
              <Line type="monotone" dataKey="req" name="req/s" stroke={REQ} strokeWidth={2} dot={false} activeDot={{ r: 3, strokeWidth: 0 }} isAnimationActive={false} />
            </LineChart>
          </ResponsiveContainer>
        </Panel>

        <Panel title="Aggregate decode · tok/s" height="h-28">
          <ResponsiveContainer width="100%" height="100%">
            <LineChart data={chart} margin={{ top: 6, right: 8, bottom: 0, left: -8 }}>
              <CartesianGrid {...chartGrid} vertical={false} />
              {xAxis(true)}
              {yAxis()}
              <Tooltip cursor={timeCursor} content={tip({ valueFormatter: tps, labelFormatter: atConcurrency })} />
              {kneeMark(false)}
              <Line type="monotone" dataKey="tok" name="tok/s" stroke={TOK} strokeWidth={2} dot={false} activeDot={{ r: 3, strokeWidth: 0 }} isAnimationActive={false} />
            </LineChart>
          </ResponsiveContainer>
        </Panel>
        <div className="pl-[46px] text-center text-[10px] text-muted-foreground">concurrency</div>
      </div>

      <div className="grid grid-cols-3 gap-2 text-[11.5px] lg:grid-cols-5">
        {steps.map((s) => (
          <div key={s.concurrency} className="rounded-md bg-white/[0.025] p-2">
            <div className="font-medium tabular-nums">×{s.concurrency}</div>
            <div className="tabular-nums text-muted-foreground">{s.reqPerS.toFixed(1)} req/s</div>
            <div className="tabular-nums text-muted-foreground">p99 {s.p99TtfbMs}ms</div>
            <div className="tabular-nums text-muted-foreground">
              {(s.tokensPerS ?? 0).toFixed(0)} tok/s
              {(s.p50DecodeTps ?? 0) > 0 && ` · ${s.p50DecodeTps.toFixed(0)}/stream`}
            </div>
            {(s.errors > 0 || s.rejected > 0) && (
              <div className="tabular-nums text-muted-foreground">{s.errors} err · {s.rejected} rej</div>
            )}
          </div>
        ))}
      </div>

      {(r.findings?.length ?? 0) > 0 && (
        <ul className="list-disc space-y-0.5 pl-4 text-[12px] text-muted-foreground">
          {r.findings!.map((f, i) => <li key={i}>{f}</li>)}
        </ul>
      )}
    </Rail>
  );
}
