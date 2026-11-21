"use client";

import { memo, useMemo, useState } from "react";
import { Area, Bar, BarChart, ComposedChart, Line, ResponsiveContainer, Tooltip, XAxis, YAxis, CartesianGrid } from "recharts";
import { TooltipRow, TooltipShell } from "@/components/chart-tooltip";
import { BUCKET_LABEL, PREVIOUS_LABEL, RANGE_BUCKET_MS, RANGE_LABEL, type OverviewRange, type SeriesPoint } from "@/lib/overview-model";
import { clock, compact, formatDate, formatMs, formatNumber } from "@/lib/format";
import { Panel, Segmented } from "./ui";
import { activeDot, axisTick, barCursor, chartGrid, seriesCurrent, seriesPrevious, timeCursor } from "@/lib/chart-style";

type Measure = "requests" | "tokens" | "latency";

/** Callers' failures: the platform's bar colour, light. */
const CALLER_OPACITY = 0.28;

function timeLabel(t: number, range: OverviewRange) {
  const d = new Date(t);
  return range === "7d"
    ? formatDate(d, { weekday: "short", hour: "numeric", minute: "2-digit" })
    : clock(d);
}

interface Row { t: number; label: string; cur: number | null; prev: number | null; errors: number; platform: number; caller: number; peak: boolean }

function TrafficTooltip({ active, payload, measure, range, split }: { active?: boolean; payload?: { payload: Row }[]; measure: Measure; range: OverviewRange; split: boolean }) {
  const row = payload?.[0]?.payload;
  if (!active || !row) return null;
  const fmt = (v: number | null) => (v === null ? "—" : measure === "latency" ? formatMs(v) : formatNumber(v));
  const end = timeLabel(row.t + RANGE_BUCKET_MS[range], range);
  return (
    <TooltipShell title={`${row.label}–${end}`}>
      <TooltipRow label="This window" value={fmt(row.cur)} />
      <TooltipRow muted label={PREVIOUS_LABEL[range].replace(/^\w/, (c) => c.toUpperCase())} value={fmt(row.prev)} />
      {split ? (
        <>
          <TooltipRow className="mt-1" label="Failed on the platform" value={formatNumber(row.platform)} />
          <TooltipRow muted label="Failed from callers" value={formatNumber(row.caller)} />
        </>
      ) : (
        <TooltipRow muted className="mt-1" label="Failed" value={formatNumber(row.errors)} />
      )}
    </TooltipShell>
  );
}

/**
 * One measure at a time on one axis — the previous window dashed behind it —
 * with failed requests as their own small chart underneath, sharing the
 * time axis and the hover: the platform's failures solid, the callers'
 * stacked on them light. A gateway that does not split them shows every
 * failure solid.
 */
export const TrafficPanel = memo(function TrafficPanel({ series, range, hasErrors, split = false }: { series: SeriesPoint[]; range: OverviewRange; hasErrors: boolean; split?: boolean }) {
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
      platform: split ? p.platformErrors : p.errors,
      caller: split ? Math.max(0, p.errors - p.platformErrors) : 0,
      // The peak named under the chart fails at least 3× the typical failing bucket.
      peak: p.errors > 0 && p.errors >= Math.max(10, median * 3),
    }));
  }, [series, measure, range, split]);
  const totalErrors = rows.reduce((n, r) => n + r.errors, 0);
  const totalPlatform = rows.reduce((n, r) => n + r.platform, 0);
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
                  <YAxis tick={axisTick} axisLine={false} tickLine={false} width={44} allowDecimals={false} tickFormatter={measure === "latency" ? (v: number) => formatMs(v) : compact} />
                  <Tooltip cursor={timeCursor} content={<TrafficTooltip measure={measure} range={range} split={split} />} />
                  <Area type="monotone" dataKey="cur" name="This window" stroke="none" fill={seriesCurrent} fillOpacity={0.07} isAnimationActive={false} connectNulls={measure !== "latency"} />
                  <Line type="monotone" dataKey="prev" name="Previous" stroke={seriesPrevious} strokeWidth={2} strokeDasharray="5 4" dot={false} activeDot={activeDot} isAnimationActive={false} connectNulls={measure !== "latency"} />
                  <Line type="monotone" dataKey="cur" name="This window" stroke={seriesCurrent} strokeWidth={2} dot={false} activeDot={activeDot} isAnimationActive={false} connectNulls={measure !== "latency"} />
                </ComposedChart>
              </ResponsiveContainer>
            </div>
            <div className="flex items-center justify-between pl-11 pt-1 text-xs">
              <span className="flex flex-wrap items-center gap-x-3 text-secondary-foreground">
                Failed requests
                {hasErrors && split && (
                  <span className="flex items-center gap-3 text-muted-foreground">
                    <span className="inline-flex items-center gap-1.5"><span className="h-2 w-2 rounded-[2px]" style={{ background: seriesCurrent }} />Platform {formatNumber(totalPlatform)}</span>
                    <span className="inline-flex items-center gap-1.5"><span className="h-2 w-2 rounded-[2px]" style={{ background: seriesCurrent, opacity: CALLER_OPACITY }} />Callers {formatNumber(totalErrors - totalPlatform)}</span>
                  </span>
                )}
              </span>
              <span className="text-muted-foreground">
                {!hasErrors ? "Needs a newer gateway" : peak ? <>Peak <span className="text-foreground">{formatNumber(peak.errors)}</span> at {peak.label} · {formatNumber(totalErrors)} total</> : `${formatNumber(totalErrors)} total`}
              </span>
            </div>
            <div className="h-[72px]">
              <ResponsiveContainer width="100%" height="100%">
                <BarChart data={rows} syncId="overview-traffic" margin={{ top: 4, right: 4, left: 0, bottom: 0 }} barCategoryGap={2}>
                  <XAxis dataKey="label" tick={axisTick} axisLine={false} tickLine={false} minTickGap={48} height={20} />
                  <YAxis width={44} tick={false} axisLine={false} tickLine={false} allowDecimals={false} tickFormatter={compact} />
                  <Tooltip cursor={barCursor} content={<TrafficTooltip measure={measure} range={range} split={split} />} />
                  <Bar dataKey="platform" name="Failed on the platform" stackId="failed" fill={seriesCurrent} isAnimationActive={false} minPointSize={0} />
                  <Bar dataKey="caller" name="Failed from callers" stackId="failed" fill={seriesCurrent} fillOpacity={CALLER_OPACITY} radius={[2, 2, 0, 0]} isAnimationActive={false} minPointSize={0} />
                </BarChart>
              </ResponsiveContainer>
            </div>
          </>
        )}
      </div>
    </Panel>
  );
});
