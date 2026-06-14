"use client";

import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  CartesianGrid,
  ComposedChart,
  Line,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import { axisTick, chartGrid, ChartShell, compactAxis, tip, timeCursor } from "@/components/chart-tooltip";
import type { ModelUsageTimePoint, UsageBreakdownEntry } from "@/lib/obleth";
import { cn, formatNumber } from "@/lib/utils";

export type ModelWindow = "5m" | "15m" | "1h" | "6h" | "24h";

const HOUR_MS = 3_600_000;
const DAY_MS = 86_400_000;
const USAGE_POLL_MS = 20_000;

/** Lookback per window, in ms. */
export const WINDOW_MS: Record<ModelWindow, number> = {
  "5m": 5 * 60_000,
  "15m": 15 * 60_000,
  "1h": HOUR_MS,
  "6h": 6 * HOUR_MS,
  "24h": DAY_MS,
};

/** Time-bucket width per window for the per-model series charts. Backend clamps
 * to a 10s floor, so the tightest window still yields a readable line. */
export const WINDOW_BUCKET_MS: Record<ModelWindow, number> = {
  "5m": 15_000,
  "15m": 30_000,
  "1h": 60_000,
  "6h": 300_000,
  "24h": 900_000,
};

const WINDOW_ORDER: ModelWindow[] = ["5m", "15m", "1h", "6h", "24h"];

const GEN_COLOR = "hsl(205 60% 58%)";
const PROMPT_COLOR = "hsl(350 55% 62%)";
const P50_COLOR = "hsl(160 45% 55%)";
const AVG_COLOR = "hsl(38 65% 60%)";

async function getJson<T>(url: string): Promise<T> {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`fetch failed: ${url}`);
  return (await res.json()) as T;
}

function formatCompact(n: number): string {
  if (!Number.isFinite(n)) return "0";
  const sign = n < 0 ? "-" : "";
  const abs = Math.abs(n);
  if (abs < 1000) return `${sign}${abs < 10 ? String(Math.round(abs * 10) / 10) : String(Math.round(abs))}`;
  if (abs < 1_000_000) return `${sign}${(abs / 1000).toFixed(abs < 10_000 ? 1 : 0)}k`;
  if (abs < 1_000_000_000) return `${sign}${(abs / 1_000_000).toFixed(abs < 10_000_000 ? 1 : 0)}M`;
  return `${sign}${(abs / 1_000_000_000).toFixed(1)}B`;
}

function formatDecimal(n: number): string {
  if (!Number.isFinite(n)) return "0";
  if (Math.abs(n) >= 10 || Number.isInteger(n)) return formatNumber(Math.round(n));
  return n.toFixed(1);
}

function EmptyMessage({ children, className = "h-24" }: { children: React.ReactNode; className?: string }) {
  return (
    <div className={cn("flex items-center justify-center rounded-sm border border-dashed border-border/70 text-xs text-muted-foreground", className)}>
      {children}
    </div>
  );
}

/** Per-model live traffic: a window selector driving the tenant/key breakdown
 * table and the three time-series charts. Self-contained so it can be dropped
 * into the overview model card or the Models page detail panel. */
export function ModelMetricsDetail({
  model,
  defaultWindow = "1h",
}: {
  model: string;
  defaultWindow?: ModelWindow;
}) {
  const [windowKey, setWindowKey] = useState<ModelWindow>(defaultWindow);

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-end gap-2">
        <div className="inline-flex rounded-sm border border-border bg-background/40 p-0.5">
          {WINDOW_ORDER.map((w) => (
            <button
              key={w}
              type="button"
              onClick={() => setWindowKey(w)}
              className={cn(
                "rounded-sm px-2.5 py-1 text-[11px] transition-colors",
                windowKey === w ? "bg-muted/50 text-foreground" : "text-muted-foreground hover:text-foreground",
              )}
            >
              {w}
            </button>
          ))}
        </div>
      </div>
      <ModelBreakdownTable model={model} windowKey={windowKey} />
      <ModelSeriesCharts model={model} windowKey={windowKey} />
    </div>
  );
}

/** Per-tenant/key breakdown of one model's traffic over the active window. */
function ModelBreakdownTable({ model, windowKey }: { model: string; windowKey: ModelWindow }) {
  const windowMs = WINDOW_MS[windowKey];
  const query = useQuery({
    queryKey: ["usage-breakdown", model, windowKey],
    queryFn: () =>
      getJson<UsageBreakdownEntry[]>(
        `/api/live/usage/breakdown?model=${encodeURIComponent(model)}&since_ms=${Date.now() - windowMs}&limit=25`,
      ),
    refetchInterval: USAGE_POLL_MS,
  });

  const rows = query.data ?? [];
  const windowSecs = windowMs / 1000;

  return (
    <div>
      <p className="mb-2 text-xs font-medium">Tenant / key breakdown</p>
      {query.isLoading && rows.length === 0 ? (
        <div className="text-xs text-muted-foreground">Loading breakdown…</div>
      ) : rows.length === 0 ? (
        <EmptyMessage>No tenant traffic for this model in the window</EmptyMessage>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full min-w-[560px] text-xs">
            <thead>
              <tr className="border-b border-border text-[10px] uppercase tracking-wider text-muted-foreground">
                <th className="py-1.5 pr-2 text-left font-medium">Tenant / Key</th>
                <th className="py-1.5 pr-2 text-left font-medium">Group</th>
                <th className="py-1.5 pr-2 text-right font-medium">Requests</th>
                <th className="py-1.5 pr-2 text-right font-medium">Req/s</th>
                <th className="py-1.5 pr-2 text-right font-medium">Tokens</th>
                <th className="py-1.5 text-right font-medium">Gen tok/s</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((r) => {
                const keyLabel = r.key_prefix || r.key_name || `${r.key_id.slice(0, 8)}…`;
                const tenantLabel = r.tenant_name || `${r.tenant_id.slice(0, 8)}…`;
                return (
                  <tr key={r.key_id} className="border-b border-border/60">
                    <td className="py-1.5 pr-2">
                      <span className="font-medium">{tenantLabel}</span>{" "}
                      <span className="text-muted-foreground">· {keyLabel}</span>
                    </td>
                    <td className="py-1.5 pr-2 text-muted-foreground">{r.fairshare_group || "default"}</td>
                    <td className="py-1.5 pr-2 text-right tabular-nums">{formatNumber(r.requests)}</td>
                    <td className="py-1.5 pr-2 text-right tabular-nums">{formatDecimal(r.requests / windowSecs)}</td>
                    <td className="py-1.5 pr-2 text-right tabular-nums">{formatCompact(r.total_tokens)}</td>
                    <td className="py-1.5 text-right tabular-nums">{formatDecimal(r.gen_tokens_per_sec)}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

/** Three time-series charts (throughput, E2E latency, time-to-first-byte) for one model. */
function ModelSeriesCharts({ model, windowKey }: { model: string; windowKey: ModelWindow }) {
  const query = useQuery({
    queryKey: ["usage-series-model", model, windowKey],
    queryFn: () =>
      getJson<ModelUsageTimePoint[]>(
        `/api/live/usage/series/models?model=${encodeURIComponent(model)}&bucket_ms=${WINDOW_BUCKET_MS[windowKey]}&since_ms=${Date.now() - WINDOW_MS[windowKey]}`,
      ),
    refetchInterval: USAGE_POLL_MS,
  });

  const data = useMemo(
    () =>
      (query.data ?? [])
        .slice()
        .sort((a, b) => a.bucket_ms - b.bucket_ms)
        .map((p) => ({
          time: new Date(p.bucket_ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }),
          gen: Number(p.gen_tokens_per_sec),
          prompt: Number(p.prompt_tokens_per_sec),
          ttftAvg: Number(p.avg_ttft_ms),
          ttftP50: Number(p.p50_ttft_ms),
          e2eAvg: Number(p.avg_total_ms),
          e2eP50: Number(p.p50_total_ms),
        })),
    [query.data],
  );

  if (query.isLoading && data.length === 0) {
    return <div className="text-xs text-muted-foreground">Loading charts…</div>;
  }
  if (data.length === 0) {
    return <EmptyMessage className="h-40">No series data in this window</EmptyMessage>;
  }

  return (
    <div className="grid gap-3 lg:grid-cols-3">
      <SeriesChartCard
        title="Token throughput (tok/s)"
        subtitle="per-stream median · prefill vs decode rate"
        data={data}
        valueFormatter={(v) => `${formatDecimal(v)} tok/s`}
        lines={[
          { key: "gen", name: "Generation", color: GEN_COLOR },
          { key: "prompt", name: "Prompt", color: PROMPT_COLOR },
        ]}
      />
      <SeriesChartCard
        title="E2E latency (ms)"
        subtitle="gateway-observed, wall-clock"
        data={data}
        valueFormatter={(v) => `${formatNumber(Math.round(v))} ms`}
        lines={[
          { key: "e2eP50", name: "p50", color: P50_COLOR },
          { key: "e2eAvg", name: "avg", color: AVG_COLOR, dashed: true },
        ]}
      />
      <SeriesChartCard
        title="Time to first byte (ms)"
        subtitle="gateway → first upstream byte"
        data={data}
        valueFormatter={(v) => `${formatNumber(Math.round(v))} ms`}
        lines={[
          { key: "ttftP50", name: "p50", color: P50_COLOR },
          { key: "ttftAvg", name: "avg", color: AVG_COLOR, dashed: true },
        ]}
      />
    </div>
  );
}

interface SeriesLine {
  key: string;
  name: string;
  color: string;
  dashed?: boolean;
}

function SeriesChartCard({
  title,
  subtitle,
  data,
  lines,
  valueFormatter,
}: {
  title: string;
  subtitle?: string;
  data: Array<Record<string, number | string>>;
  lines: SeriesLine[];
  valueFormatter: (v: number) => string;
}) {
  return (
    <div className="rounded-sm border border-border bg-background/30 p-3">
      <p className="text-xs font-medium">{title}</p>
      {subtitle ? <p className="mb-2 text-[10px] text-muted-foreground">{subtitle}</p> : <div className="mb-2" />}
      <ChartShell heightClass="h-44">
        <ResponsiveContainer width="100%" height="100%">
          <ComposedChart data={data} margin={{ top: 6, right: 8, left: 0, bottom: 4 }}>
            <CartesianGrid {...chartGrid} vertical={false} />
            <XAxis dataKey="time" tick={axisTick} axisLine={false} tickLine={false} minTickGap={32} />
            <YAxis tick={axisTick} axisLine={false} tickLine={false} width={40} allowDecimals={false} tickFormatter={compactAxis} />
            <Tooltip cursor={timeCursor} content={tip({ valueFormatter })} />
            {lines.map((l) => (
              <Line
                key={l.key}
                type="monotone"
                dataKey={l.key}
                name={l.name}
                stroke={l.color}
                strokeWidth={1.5}
                strokeDasharray={l.dashed ? "4 3" : undefined}
                dot={false}
                activeDot={{ r: 3, strokeWidth: 0 }}
                isAnimationActive={false}
              />
            ))}
          </ComposedChart>
        </ResponsiveContainer>
      </ChartShell>
      <div className="mt-1.5 flex justify-center gap-4 text-[10px] text-muted-foreground">
        {lines.map((l) => (
          <span key={l.key} className="inline-flex items-center gap-1">
            <span className="inline-block h-0.5 w-3" style={{ background: l.color }} />
            {l.name}
          </span>
        ))}
      </div>
    </div>
  );
}
