"use client";

import { memo, useMemo, useState } from "react";
import { Area, Bar, BarChart, Cell, ComposedChart, Line, ResponsiveContainer, Tooltip, XAxis, YAxis, CartesianGrid } from "recharts";
import { axisTick, chartGrid, compactAxis, timeCursor } from "@/components/chart-tooltip";
import { BUCKET_LABEL, PREVIOUS_LABEL, RANGE_BUCKET_MS, RANGE_LABEL, compact, formatMs, type OverviewRange, type SeriesPoint } from "@/lib/overview-model";
import { Panel, Segmented } from "./ui";

type Measure = "requests" | "tokens" | "latency";

const CURRENT = "hsl(240 5% 90%)";
const PREVIOUS = "hsl(240 4% 46%)";

function timeLabel(t: number, range: OverviewRange) {
  const d = new Date(t);
  return range === "7d"
    ? d.toLocaleString([], { weekday: "short", hour: "2-digit", minute: "2-digit" })
    : d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

interface Row { t: number; label: string; cur: number | null; prev: number | null; errors: number; peak: boolean }

function TrafficTooltip({ active, payload, measure, range }: { active?: boolean; payload?: { payload: Row }[]; measure: Measure; range: OverviewRange }) {
  const row = payload?.[0]?.payload;
  if (!active || !row) return null;
  const fmt = (v: number | null) => (v === null ? "—" : measure === "latency" ? formatMs(v) : v.toLocaleString());
  const end = timeLabel(row.t + RANGE_BUCKET_MS[range], range);
  return (
    <div className="min-w-44 rounded-lg border border-border bg-[hsl(240_5%_9%)] px-3 py-2 text-xs shadow-xl">
      <p className="mb-1.5 font-mono text-muted-foreground">{row.label}–{end}</p>
      <div className="flex justify-between gap-6"><span>This window</span><span className="font-mono tabular-nums">{fmt(row.cur)}</span></div>
      <div className="flex justify-between gap-6 text-muted-foreground"><span>{PREVIOUS_LABEL[range].replace(/^\w/, (c) => c.toUpperCase())}</span><span className="font-mono tabular-nums">{fmt(row.prev)}</span></div>
      <div className="mt-1 flex justify-between gap-6 text-muted-foreground"><span>Failed</span><span className="font-mono tabular-nums">{row.errors.toLocaleString()}</span></div>
    </div>
  );
}

/**
 * One measure at a time on one axis — the previous window dashed behind it —
 * with failed requests as their own small chart underneath, sharing the
 * time axis and the hover.
 */
export const TrafficPanel = memo(function TrafficPanel({ series, range, hasErrors }: { series: SeriesPoint[]; range: OverviewRange; hasErrors: boolean }) {
  const [measure, setMeasure] = useState<Measure>("requests");
  const rows = useMemo<Row[]>(() => {
    const errs = series.map((p) => p.errors).filter((e) => e > 0).sort((a, b) => a - b);
    const median = errs.length ? errs[Math.floor(errs.length / 2)] : 0;
    return series.map((p) => ({
      t: p.t,
      label: timeLabel(p.t, range),
      cur: measure === "requests" ? p.requests : measure === "tokens" ? p.tokens : p.ttft,
      prev: measure === "requests" ? p.prevRequests : measure === "tokens" ? p.prevTokens : p.prevTtft,
      errors: p.errors,
      // A bucket stands out when it fails at least 3× the typical failing bucket.
      peak: p.errors > 0 && p.errors >= Math.max(10, median * 3),
    }));
  }, [series, measure, range]);
  const totalErrors = rows.reduce((n, r) => n + r.errors, 0);
  const peak = rows.reduce<Row | null>((best, r) => (r.peak && (!best || r.errors > best.errors) ? r : best), null);
  const empty = rows.every((r) => !r.cur && !r.prev);
  const title = measure === "requests" ? "Requests" : measure === "tokens" ? "Tokens" : "Time to first token";

  return (
    <Panel
      label="Traffic"
      title={title}
      subtitle={`${BUCKET_LABEL[range]}, ${RANGE_LABEL[range].toLowerCase()}${measure === "latency" ? " · median per bucket" : ""}`}
      action={
        <div className="flex flex-wrap items-center gap-3">
          <span className="flex items-center gap-3 text-xs text-muted-foreground">
            <span className="inline-flex items-center gap-1.5"><span className="h-0.5 w-4 rounded bg-foreground" />This window</span>
            <span className="inline-flex items-center gap-1.5"><span className="w-4 border-t-2 border-dashed border-muted-foreground" />{PREVIOUS_LABEL[range].replace(/^\w/, (c) => c.toUpperCase())}</span>
          </span>
          <Segmented label="Measure" value={measure} onChange={setMeasure} options={[{ value: "requests", label: "Requests" }, { value: "tokens", label: "Tokens" }, { value: "latency", label: "Latency" }]} />
        </div>
      }
    >
      <div className="flex flex-col gap-1 px-[18px] pb-4 pt-3">
        {empty ? (
          <div className="flex h-[228px] items-center justify-center rounded-lg border border-dashed border-border text-sm text-muted-foreground">No traffic in this window</div>
        ) : (
          <>
            <div className="h-[200px]">
              <ResponsiveContainer width="100%" height="100%">
                <ComposedChart data={rows} syncId="overview-traffic" margin={{ top: 8, right: 4, left: 0, bottom: 0 }}>
                  <CartesianGrid {...chartGrid} vertical={false} />
                  <XAxis dataKey="label" hide />
                  <YAxis tick={axisTick} axisLine={false} tickLine={false} width={44} allowDecimals={false} tickFormatter={measure === "latency" ? (v: number) => formatMs(v) : compactAxis} />
                  <Tooltip cursor={timeCursor} content={<TrafficTooltip measure={measure} range={range} />} />
                  <Area type="monotone" dataKey="cur" name="This window" stroke="none" fill={CURRENT} fillOpacity={0.07} isAnimationActive={false} connectNulls={measure !== "latency"} />
                  <Line type="monotone" dataKey="prev" name="Previous" stroke={PREVIOUS} strokeWidth={2} strokeDasharray="5 4" dot={false} activeDot={{ r: 4, strokeWidth: 2, stroke: "hsl(240 5% 7%)" }} isAnimationActive={false} connectNulls={measure !== "latency"} />
                  <Line type="monotone" dataKey="cur" name="This window" stroke={CURRENT} strokeWidth={2} dot={false} activeDot={{ r: 4, strokeWidth: 2, stroke: "hsl(240 5% 7%)" }} isAnimationActive={false} connectNulls={measure !== "latency"} />
                </ComposedChart>
              </ResponsiveContainer>
            </div>
            <div className="flex items-center justify-between pl-11 pt-1 text-xs">
              <span className="text-secondary-foreground">Failed requests</span>
              <span className="text-muted-foreground">
                {!hasErrors ? "Needs a newer gateway" : peak ? <>Peak <span className="text-foreground">{peak.errors.toLocaleString()}</span> at {peak.label} · {totalErrors.toLocaleString()} total</> : `${totalErrors.toLocaleString()} total`}
              </span>
            </div>
            <div className="h-[72px]">
              <ResponsiveContainer width="100%" height="100%">
                <BarChart data={rows} syncId="overview-traffic" margin={{ top: 4, right: 4, left: 0, bottom: 0 }} barCategoryGap={2}>
                  <XAxis dataKey="label" tick={axisTick} axisLine={false} tickLine={false} minTickGap={48} height={20} />
                  <YAxis width={44} tick={false} axisLine={false} tickLine={false} allowDecimals={false} tickFormatter={compact} />
                  <Tooltip cursor={{ fill: "hsl(240 4% 16% / 0.5)" }} content={<TrafficTooltip measure={measure} range={range} />} />
                  <Bar dataKey="errors" name="Failed" radius={[2, 2, 0, 0]} isAnimationActive={false} minPointSize={0}>
                    {rows.map((r) => <Cell key={r.t} fill={r.peak ? CURRENT : PREVIOUS} />)}
                  </Bar>
                </BarChart>
              </ResponsiveContainer>
            </div>
          </>
        )}
      </div>
    </Panel>
  );
});
