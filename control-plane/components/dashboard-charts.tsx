"use client";

import Link from "next/link";
import { useMemo, useState, type ReactNode } from "react";
import {
  Area,
  Bar,
  BarChart,
  CartesianGrid,
  Cell,
  ComposedChart,
  Legend,
  Line,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { axisTick, chartGrid, ChartShell, compactAxis, tip, timeCursor } from "@/components/chart-tooltip";
import { formatCurrency, formatNumber } from "@/lib/utils";

const TOP_TENANTS = 10;
const TOP_MODELS = 10;

const PALETTE = [
  "hsl(158 42% 48%)",
  "hsl(205 55% 58%)",
  "hsl(38 60% 56%)",
  "hsl(278 42% 62%)",
  "hsl(350 50% 60%)",
  "hsl(185 42% 52%)",
];

const TOKEN_COLOR = "hsl(205 55% 58%)";
const REQUESTS_COLOR = "hsl(38 75% 60%)";
const OTHERS_COLOR = "hsl(240 6% 42%)";

const CHART_H = "h-64";
const BAR_CHART_H = "h-72";

export interface VolumePoint {
  time: string;
  tokens: number;
  requests: number;
}

export interface TenantUsageRow {
  id: string;
  name: string;
  requests: number;
  total_tokens: number;
}

export interface ModelUsageRow {
  model: string;
  requests: number;
  total_tokens: number;
}

export interface AuditPreviewRow {
  id: number;
  ts: string;
  actor: string;
  action: string;
  entity_type: string;
}

// ---------------------------------------------------------------------------
// KPI strip
// ---------------------------------------------------------------------------

export function OverviewKpiStrip({
  requests,
  tokens,
  cost,
  hasPricing,
  tenantCount,
  activeTenants,
  modelCount,
  enabledModels,
  keyCount,
}: {
  requests: number;
  tokens: number;
  cost: number;
  hasPricing: boolean;
  tenantCount: number;
  activeTenants: number;
  modelCount: number;
  enabledModels: number;
  keyCount: number;
}) {
  const avgTokens = requests > 0 ? Math.round(tokens / requests) : 0;
  const items = [
    { label: "Requests (24h)", value: formatNumber(requests), sub: "completed" },
    { label: "Tokens (24h)", value: formatNumber(tokens), sub: avgTokens > 0 ? `~${formatNumber(avgTokens)} / request` : "input + output" },
    {
      label: "Est. cost (24h)",
      value: hasPricing ? formatCurrency(cost) : "—",
      sub: hasPricing ? "from model pricing" : "set pricing on Models",
    },
    { label: "Tenants", value: formatNumber(tenantCount), sub: `${formatNumber(activeTenants)} with traffic` },
    { label: "Models", value: formatNumber(enabledModels), sub: `${modelCount} configured` },
    { label: "API keys", value: formatNumber(keyCount), sub: "registered" },
  ];
  return (
    <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-6">
      {items.map((item) => (
        <div key={item.label} className="rounded-lg border border-border bg-card/50 px-4 py-3">
          <p className="text-[10px] uppercase tracking-wider text-muted-foreground">{item.label}</p>
          <p className="mt-0.5 text-lg font-semibold tabular-nums">{item.value}</p>
          <p className="text-[10px] tabular-nums text-muted-foreground/70">{item.sub}</p>
        </div>
      ))}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Quick links into operational pages
// ---------------------------------------------------------------------------

const QUICK_LINKS = [
  { href: "/fairshare", title: "Fairshare", desc: "Live scheduler, slots, and contention" },
  { href: "/tenants", title: "Tenants", desc: "Weights, limits, and groups" },
  { href: "/keys", title: "API keys", desc: "Credentials and key usage" },
  { href: "/models", title: "Models", desc: "Routes, pricing, and admission weight" },
];

export function QuickLinks() {
  return (
    <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
      {QUICK_LINKS.map((link) => (
        <Link
          key={link.href}
          href={link.href}
          className="rounded-lg border border-border bg-card/50 px-4 py-3 transition-colors hover:border-muted-foreground/40 hover:bg-card"
        >
          <p className="text-sm font-medium">{link.title}</p>
          <p className="mt-0.5 text-xs text-muted-foreground">{link.desc}</p>
        </Link>
      ))}
    </div>
  );
}

// ---------------------------------------------------------------------------
// 24h volume
// ---------------------------------------------------------------------------

export function VolumeChart({ series }: { series: VolumePoint[] }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>Traffic (24h)</CardTitle>
        <CardDescription>Token volume and request count · 5-minute buckets</CardDescription>
      </CardHeader>
      <CardContent>
        {series.length === 0 ? (
          <EmptyState className={CHART_H}>No traffic in the last 24 hours</EmptyState>
        ) : (
          <ChartShell heightClass={CHART_H}>
            <ResponsiveContainer width="100%" height="100%">
              <ComposedChart data={series} margin={{ top: 8, right: 12, left: 4, bottom: 28 }}>
                <defs>
                  <linearGradient id="vol-tokens" x1="0" y1="0" x2="0" y2="1">
                    <stop offset="0%" stopColor={TOKEN_COLOR} stopOpacity={0.55} />
                    <stop offset="100%" stopColor={TOKEN_COLOR} stopOpacity={0.05} />
                  </linearGradient>
                </defs>
                <CartesianGrid {...chartGrid} vertical={false} />
                <XAxis dataKey="time" tick={axisTick} axisLine={false} tickLine={false} minTickGap={48} />
                <YAxis
                  yAxisId="tokens"
                  tick={axisTick}
                  axisLine={false}
                  tickLine={false}
                  width={44}
                  allowDecimals={false}
                  tickFormatter={compactAxis}
                />
                <YAxis
                  yAxisId="requests"
                  orientation="right"
                  tick={{ ...axisTick, fill: REQUESTS_COLOR }}
                  axisLine={false}
                  tickLine={false}
                  width={40}
                  allowDecimals={false}
                />
                <Tooltip cursor={timeCursor} content={tip()} />
                <Legend wrapperStyle={{ fontSize: 11 }} />
                <Area
                  yAxisId="tokens"
                  type="monotone"
                  dataKey="tokens"
                  name="Tokens"
                  stroke={TOKEN_COLOR}
                  fill="url(#vol-tokens)"
                  strokeWidth={1.5}
                  isAnimationActive={false}
                  dot={false}
                  activeDot={{ r: 3, strokeWidth: 0 }}
                />
                <Line
                  yAxisId="requests"
                  type="monotone"
                  dataKey="requests"
                  name="Requests"
                  stroke={REQUESTS_COLOR}
                  strokeWidth={1.5}
                  strokeDasharray="4 3"
                  dot={false}
                  activeDot={{ r: 3, strokeWidth: 0 }}
                  isAnimationActive={false}
                />
              </ComposedChart>
            </ResponsiveContainer>
          </ChartShell>
        )}
      </CardContent>
    </Card>
  );
}

// ---------------------------------------------------------------------------
// Top tenants / models
// ---------------------------------------------------------------------------

export function TenantUsageChart({ tenants }: { tenants: TenantUsageRow[] }) {
  const data = useMemo(() => buildTopBar(tenants, TOP_TENANTS, (t) => t.name, (t) => t.requests, (t) => t.total_tokens), [tenants]);

  return (
    <Card className="h-full">
      <CardHeader>
        <CardTitle>Top tenants</CardTitle>
        <CardDescription>By request volume · last 24 hours</CardDescription>
      </CardHeader>
      <CardContent>
        {data.length === 0 ? (
          <EmptyState className={BAR_CHART_H}>No tenant traffic yet</EmptyState>
        ) : (
          <HorizontalBarChart
            data={data}
            valueKey="requests"
            valueName="Requests"
            tooltipExtra={(row) => `${formatNumber(row.tokens as number)} tokens`}
          />
        )}
      </CardContent>
    </Card>
  );
}

export function ModelUsageChart({ models }: { models: ModelUsageRow[] }) {
  const data = useMemo(
    () => buildTopBar(models, TOP_MODELS, (m) => m.model, (m) => m.requests, (m) => m.total_tokens),
    [models],
  );

  return (
    <Card className="h-full">
      <CardHeader>
        <CardTitle>Top models</CardTitle>
        <CardDescription>By request volume · last 24 hours</CardDescription>
      </CardHeader>
      <CardContent>
        {data.length === 0 ? (
          <EmptyState className={BAR_CHART_H}>No model traffic yet</EmptyState>
        ) : (
          <HorizontalBarChart
            data={data}
            valueKey="requests"
            valueName="Requests"
            tooltipExtra={(row) => `${formatNumber(row.tokens as number)} tokens`}
          />
        )}
      </CardContent>
    </Card>
  );
}

function buildTopBar<T>(
  rows: T[],
  limit: number,
  label: (row: T) => string,
  value: (row: T) => number,
  extra: (row: T) => number,
) {
  const sorted = [...rows].filter((r) => value(r) > 0).sort((a, b) => value(b) - value(a));
  const top = sorted.slice(0, limit).map((row, i) => ({
    name: truncate(label(row), 18),
    requests: value(row),
    tokens: extra(row),
    fill: PALETTE[i % PALETTE.length],
  }));
  const rest = sorted.slice(limit);
  if (rest.length > 0) {
    top.push({
      name: `others (${rest.length})`,
      requests: rest.reduce((s, r) => s + value(r), 0),
      tokens: rest.reduce((s, r) => s + extra(r), 0),
      fill: OTHERS_COLOR,
    });
  }
  return top;
}

function HorizontalBarChart({
  data,
  valueKey,
  valueName,
  tooltipExtra,
}: {
  data: { name: string; fill: string; requests: number; tokens: number }[];
  valueKey: string;
  valueName: string;
  tooltipExtra: (row: Record<string, unknown>) => string;
}) {
  return (
    <ChartShell heightClass={BAR_CHART_H}>
      <ResponsiveContainer width="100%" height="100%">
        <BarChart data={data} layout="vertical" margin={{ left: 4, right: 16, top: 4, bottom: 4 }} barGap={2}>
          <CartesianGrid {...chartGrid} horizontal={false} />
          <XAxis type="number" tick={axisTick} axisLine={false} tickLine={false} allowDecimals={false} />
          <YAxis type="category" dataKey="name" width={120} tick={axisTick} axisLine={false} tickLine={false} interval={0} />
          <Tooltip
            cursor={false}
            content={tip({
              labelFormatter: (l, p) => {
                const row = p[0]?.payload as Record<string, unknown> | undefined;
                return row ? `${l} · ${tooltipExtra(row)}` : String(l);
              },
            })}
          />
          <Bar dataKey={valueKey} name={valueName} radius={[0, 4, 4, 0]} barSize={9}>
            {data.map((d) => (
              <Cell key={d.name} fill={d.fill} />
            ))}
          </Bar>
        </BarChart>
      </ResponsiveContainer>
    </ChartShell>
  );
}

// ---------------------------------------------------------------------------
// Recent config changes
// ---------------------------------------------------------------------------

export function RecentAuditPanel({ entries }: { entries: AuditPreviewRow[] }) {
  return (
    <Card>
      <CardHeader className="flex flex-row items-center justify-between gap-4">
        <div>
          <CardTitle>Recent changes</CardTitle>
          <CardDescription>Latest configuration events from the management API</CardDescription>
        </div>
        <Link href="/audit" className="text-xs text-muted-foreground underline underline-offset-2 hover:text-foreground">
          View all
        </Link>
      </CardHeader>
      <CardContent className="p-0">
        {entries.length === 0 ? (
          <p className="px-6 py-10 text-center text-sm text-muted-foreground">No audit events yet</p>
        ) : (
          <ul className="divide-y divide-border/60">
            {entries.map((e) => (
              <li key={e.id} className="flex items-baseline justify-between gap-4 px-6 py-3 text-sm">
                <div className="min-w-0">
                  <p className="font-mono text-xs">{e.action}</p>
                  <p className="mt-0.5 truncate text-muted-foreground">
                    {e.entity_type} · {e.actor}
                  </p>
                </div>
                <time className="shrink-0 text-xs tabular-nums text-muted-foreground">
                  {new Date(e.ts).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" })}
                </time>
              </li>
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  );
}

// ---------------------------------------------------------------------------
// Shared
// ---------------------------------------------------------------------------

function truncate(s: string, max: number) {
  return s.length > max ? `${s.slice(0, max - 1)}…` : s;
}

function EmptyState({ children, className = "h-64" }: { children: ReactNode; className?: string }) {
  return (
    <p className={`flex items-center justify-center text-center text-sm text-muted-foreground ${className}`}>{children}</p>
  );
}
