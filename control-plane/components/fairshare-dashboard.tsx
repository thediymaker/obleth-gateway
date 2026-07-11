"use client";

import Link from "next/link";
import { useEffect, useMemo, useState, useTransition } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Activity, Check, LayoutDashboard, Network, Pencil, RefreshCw, Search, Users, X } from "lucide-react";
import {
  Area,
  AreaChart,
  CartesianGrid,
  ComposedChart,
  Legend,
  Line,
  ReferenceLine,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import { setWeightAction } from "@/app/actions";
import { axisTick, chartGrid, ChartShell, compactAxis, tip, timeCursor } from "@/components/chart-tooltip";
import {
  EmptyState,
  MetricCard,
  MetricToggle,
  type MetricTile,
  type MetricTone,
} from "@/components/dashboard-primitives";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import { colorForGroup, OTHERS_COLOR, PALETTE } from "@/lib/chart-palette";
import { isWaitingBelowShare } from "@/lib/fairshare";
import { clamp, formatCompact, formatDecimal, formatDelta, formatPct, formatScore } from "@/lib/format";
import type { ModelRoute } from "@/lib/obleth";
import { cn, formatNumber } from "@/lib/utils";

export interface TenantFairshareView {
  tenant_id: string;
  name: string;
  fairshare_group: string;
  weight: number;
  in_flight: number;
  queued: number;
  served_tokens: number;
  share_score: number;
  weight_share: number;
  expected_slots: number;
}

export interface GroupFairshareView {
  name: string;
  weight: number;
  in_flight: number;
  queued: number;
  slot_cap: number;
  served_tokens: number;
  share_score: number;
  weight_share: number;
  expected_slots: number;
}

export interface FairshareLiveView {
  algorithm: string;
  max_in_flight: number;
  global_in_flight: number;
  global_queued: number;
  groups: GroupFairshareView[];
  tenants: TenantFairshareView[];
  model_in_flight?: Record<string, number>;
  model_queued?: Record<string, number>;
}

interface TenantSeriesRow {
  bucket_ms: number;
  tenant_id: string;
  requests: number;
  total_tokens: number;
}

const FAIRSHARE_POLL_MS = 2000;
const THROUGHPUT_POLL_MS = 15_000;
const ROUTES_POLL_MS = 30_000;

const TOP_SERIES = 7;
const TENANT_PAGE = 120;
const MAX_HISTORY_POINTS = 120;

const QUEUED_COLOR = "hsl(38 75% 60%)";
const STARVED_COLOR = "hsl(350 65% 60%)";
const HEALTHY_COLOR = "hsl(210 8% 70%)";
const UNDER_COLOR = "hsl(205 18% 58%)";

type ThroughputMetric = "requests" | "tokens";
type TenantSort = "pressure" | "queued" | "deficit" | "served" | "score" | "weight" | "share";
type TenantScope = "all" | "active" | "waiting" | "starved";

// ---------------------------------------------------------------------------
// Root
// ---------------------------------------------------------------------------

export function FairshareDashboard({
  tenantNames,
}: {
  tenantNames: Record<string, string>;
}) {
  const queryClient = useQueryClient();
  const { data: view, groupHistory, groupKeys, isFetching } = useFairshareLive();
  const { data: tenantSeries } = useThroughputSeries();
  const { data: modelRoutes } = useModelRoutes();
  const [throughputMetric, setThroughputMetric] = useState<ThroughputMetric>("requests");

  const summary = useMemo(() => summarizeFairshare(view), [view]);
  const throughput = useMemo(
    () => buildThroughput(tenantSeries ?? [], tenantNames, throughputMetric),
    [tenantSeries, tenantNames, throughputMetric],
  );
  const refresh = () => {
    queryClient.invalidateQueries({ queryKey: ["fairshare-live"] });
    queryClient.invalidateQueries({ queryKey: ["usage-series-tenants"] });
    queryClient.invalidateQueries({ queryKey: ["model-routes"] });
  };

  return (
    <div className="space-y-6">
      <LiveConsoleHeader
        view={view}
        summary={summary}
        isFetching={isFetching}
        onRefresh={refresh}
      />

      <FairshareSectionNav />

      <section id="live" className="scroll-mt-20 space-y-4">
        <SectionHeader
          eyebrow="Live state"
          title="Scheduler pressure"
          description="Backlog, active work, and the next admission decision."
        />
        <PressureStrip view={view} summary={summary} />
        <div className="grid gap-4 xl:grid-cols-[minmax(21rem,0.82fr)_minmax(0,1.55fr)]">
          <SchedulerNow view={view} summary={summary} />
          <CapacityTimeline history={groupHistory} groups={groupKeys} view={view} />
        </div>
      </section>

      <section id="allocation" className="scroll-mt-20 space-y-4">
        <SectionHeader
          eyebrow="Allocation"
          title="Group pools"
          description="How groups and tenants are apportioned before an individual tenant is picked."
        />
        <GroupAllocation view={view} />
        <ModelSlotPressure view={view} routes={modelRoutes ?? []} />
      </section>

      <section id="tenants" className="scroll-mt-20 space-y-4">
        <SectionHeader
          eyebrow="Tenants"
          title="Contention ledger"
          description="Search, filter, sort, and adjust tenant fairshare weight."
        />
        <FairshareBalance view={view} />
        <TenantOperations view={view} />
        <ThroughputPanel
          data={throughput.data}
          series={throughput.series}
          metric={throughputMetric}
          onMetricChange={setThroughputMetric}
        />
      </section>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Data hooks
// ---------------------------------------------------------------------------

interface GroupHistoryPoint {
  time: string;
  queued: number;
  inFlight: number;
  [key: string]: number | string;
}

interface GroupKey {
  name: string;
  key: string;
  color: string;
}

function useFairshareLive() {
  const [groupHistory, setGroupHistory] = useState<GroupHistoryPoint[]>([]);

  const query = useQuery({
    queryKey: ["fairshare-live"],
    queryFn: async () => {
      const res = await fetch("/api/live/fairshare");
      if (!res.ok) throw new Error("fairshare unavailable");
      return (await res.json()) as FairshareLiveView;
    },
    refetchInterval: FAIRSHARE_POLL_MS,
  });

  useEffect(() => {
    if (!query.data) return;
    const time = new Date().toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
    const point: GroupHistoryPoint = {
      time,
      queued: query.data.global_queued,
      inFlight: query.data.global_in_flight,
    };
    for (const g of query.data.groups) point[groupDataKey(g.name)] = g.in_flight;
    setGroupHistory((prev) => [...prev, point].slice(-MAX_HISTORY_POINTS));
  }, [query.data]);

  const groupKeys = useMemo<GroupKey[]>(
    () =>
      [...(query.data?.groups ?? [])]
        .sort((a, b) => a.name.localeCompare(b.name))
        .map((g, i) => ({
          name: g.name,
          key: groupDataKey(g.name),
          color: colorForGroup(g.name, i),
        })),
    [query.data],
  );

  return {
    data: query.data,
    groupHistory,
    groupKeys,
    isFetching: query.isFetching,
    dataUpdatedAt: query.dataUpdatedAt,
  };
}

function useThroughputSeries() {
  return useQuery({
    queryKey: ["usage-series-tenants"],
    queryFn: async () => {
      const since = Date.now() - 1_800_000;
      const res = await fetch(`/api/live/usage/tenants?bucket_ms=10000&since_ms=${since}`);
      if (!res.ok) throw new Error("usage series unavailable");
      return (await res.json()) as TenantSeriesRow[];
    },
    refetchInterval: THROUGHPUT_POLL_MS,
  });
}

function useModelRoutes() {
  return useQuery({
    queryKey: ["model-routes"],
    queryFn: async () => {
      const res = await fetch("/api/live/models");
      if (!res.ok) throw new Error("models unavailable");
      return (await res.json()) as ModelRoute[];
    },
    refetchInterval: ROUTES_POLL_MS,
  });
}

// ---------------------------------------------------------------------------
// Derived data
// ---------------------------------------------------------------------------

interface FairshareSummary {
  utilization: number;
  activeTenants: number;
  activeGroups: number;
  starvedTenants: number;
  waitingTenants: number;
  totalServed: number;
  totalTenantWeight: number;
  totalGroupWeight: number;
  nextTenant?: TenantFairshareView;
  largestDeficit?: TenantFairshareView;
}

function summarizeFairshare(view?: FairshareLiveView): FairshareSummary {
  if (!view) {
    return {
      utilization: 0,
      activeTenants: 0,
      activeGroups: 0,
      starvedTenants: 0,
      waitingTenants: 0,
      totalServed: 0,
      totalTenantWeight: 0,
      totalGroupWeight: 0,
    };
  }

  const activeTenants = view.tenants.filter((t) => isTenantActive(t)).length;
  const activeGroups = view.groups.filter((g) => g.in_flight + g.queued > 0).length;
  const waiting = view.tenants.filter((t) => t.queued > 0);
  const starved = waiting.filter(isWaitingBelowShare);
  const nextTenant = [...waiting].sort((a, b) => tenantDebt(view, a) - tenantDebt(view, b))[0];
  const largestDeficit = [...view.tenants]
    .filter((t) => t.queued > 0 || fairnessGap(t) < 0)
    .sort((a, b) => fairnessGap(a) - fairnessGap(b))[0];

  return {
    utilization: view.max_in_flight > 0 ? (view.global_in_flight / view.max_in_flight) * 100 : 0,
    activeTenants,
    activeGroups,
    starvedTenants: starved.length,
    waitingTenants: waiting.length,
    totalServed: view.tenants.reduce((sum, t) => sum + (t.served_tokens ?? 0), 0),
    totalTenantWeight: view.tenants.reduce((sum, t) => sum + (t.weight ?? 0), 0),
    totalGroupWeight: view.groups.reduce((sum, g) => sum + (g.weight ?? 0), 0),
    nextTenant,
    largestDeficit,
  };
}

interface ThroughputResult {
  data: Record<string, number | string>[];
  series: { key: string; name: string; color: string }[];
}

function buildThroughput(
  rows: TenantSeriesRow[],
  tenantNames: Record<string, string>,
  metric: ThroughputMetric,
): ThroughputResult {
  if (rows.length === 0) return { data: [], series: [] };

  const totalByTenant = new Map<string, number>();
  for (const r of rows) {
    const value = metric === "requests" ? Number(r.requests) : Number(r.total_tokens);
    totalByTenant.set(r.tenant_id, (totalByTenant.get(r.tenant_id) ?? 0) + value);
  }

  const top = [...totalByTenant.entries()]
    .sort((a, b) => b[1] - a[1])
    .slice(0, TOP_SERIES)
    .map(([id]) => id);
  const topSet = new Set(top);

  const byBucket = new Map<number, Record<string, number | string>>();
  let hasOthers = false;

  for (const r of rows) {
    const entry = byBucket.get(r.bucket_ms) ?? { ts: r.bucket_ms, others: 0 };
    const value = metric === "requests" ? Number(r.requests) : Number(r.total_tokens);
    if (topSet.has(r.tenant_id)) {
      entry[r.tenant_id] = (Number(entry[r.tenant_id]) || 0) + value;
    } else {
      entry.others = (Number(entry.others) || 0) + value;
      hasOthers = true;
    }
    byBucket.set(r.bucket_ms, entry);
  }

  const data = [...byBucket.values()]
    .sort((a, b) => (a.ts as number) - (b.ts as number))
    .map((row) => ({
      ...row,
      time: new Date(row.ts as number).toLocaleTimeString([], {
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
      }),
    }));

  const series = top.map((id, i) => ({
    key: id,
    name: tenantLabel(id, tenantNames),
    color: PALETTE[i % PALETTE.length],
  }));
  if (hasOthers) series.push({ key: "others", name: "others", color: OTHERS_COLOR });

  return { data, series };
}

// ---------------------------------------------------------------------------
// Header and pressure strip
// ---------------------------------------------------------------------------

function LiveConsoleHeader({
  view,
  summary,
  isFetching,
  onRefresh,
}: {
  view?: FairshareLiveView;
  summary: FairshareSummary;
  isFetching: boolean;
  onRefresh: () => void;
}) {
  const tone = pressureStatus(summary.utilization, view?.global_queued ?? 0);
  const label =
    tone === "hot"
      ? "Admission hot"
      : tone === "warn"
        ? (view?.global_queued ?? 0) > 0
          ? "Queue building"
          : "High usage"
        : "Scheduler clear";
  const detail = view
    ? `${formatNumber(view.global_in_flight)} active / ${formatNumber(view.global_queued)} queued / ${formatNumber(summary.waitingTenants)} waiting tenants`
    : "Waiting for live scheduler state";

  return (
    <div className="rounded-md border border-border bg-card px-4 py-3">
      <div className="flex flex-col gap-3 md:flex-row md:items-center md:justify-between">
        <div className="min-w-0">
          <h1 className="text-lg font-semibold tracking-tight">Fairshare</h1>
          <p className="mt-0.5 text-sm text-muted-foreground">Live admission, allocation, and tenant contention in one scheduler workspace.</p>
          <div className="mt-3 flex flex-wrap items-center gap-2">
            <Badge
              className={cn(
                "gap-1.5",
                tone === "hot" && "border-red-500/35 bg-red-500/10 text-red-300",
                tone === "warn" && "border-amber-500/35 bg-amber-500/10 text-amber-300",
              )}
            >
              <Activity className="h-3.5 w-3.5" />
              {label}
            </Badge>
            <Badge className="capitalize">{view?.algorithm ?? "loading"} admission</Badge>
            <Badge className="gap-1.5">
              <span className="h-1.5 w-1.5 rounded-full bg-[hsl(160_14%_58%)]" />
              poll {FAIRSHARE_POLL_MS / 1000}s
            </Badge>
          </div>
          <p className="mt-2 truncate text-xs text-muted-foreground">{detail}</p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Button type="button" variant="outline" size="sm" asChild>
            <Link href="/">
              <LayoutDashboard className="h-3.5 w-3.5" />
              Overview
            </Link>
          </Button>
          <Button type="button" variant="secondary" size="sm" onClick={onRefresh}>
            <RefreshCw className={cn("h-3.5 w-3.5", isFetching && "animate-spin")} />
            Refresh
          </Button>
        </div>
      </div>
    </div>
  );
}

function FairshareSectionNav() {
  const items = [
    { href: "#live", label: "Live pressure", icon: Activity },
    { href: "#allocation", label: "Allocation", icon: Network },
    { href: "#tenants", label: "Tenants", icon: Users },
  ];

  return (
    <div className="sticky top-2 z-20 overflow-x-auto rounded-md border border-border bg-card/95 p-2 backdrop-blur">
      <nav className="flex min-w-max items-center gap-2" aria-label="Fairshare sections">
        {items.map((item) => {
          const Icon = item.icon;
          return (
            <a
              key={item.href}
              href={item.href}
              className="inline-flex h-8 items-center gap-2 rounded-sm border border-border bg-background/35 px-3 text-xs font-medium text-muted-foreground transition-colors hover:bg-muted/20 hover:text-foreground"
            >
              <Icon className="h-3.5 w-3.5" />
              {item.label}
            </a>
          );
        })}
      </nav>
    </div>
  );
}

function SectionHeader({
  eyebrow,
  title,
  description,
}: {
  eyebrow: string;
  title: string;
  description: string;
}) {
  return (
    <div className="flex flex-col gap-1">
      <p className="text-[10px] font-medium uppercase tracking-wider text-muted-foreground">{eyebrow}</p>
      <div className="flex flex-col gap-1 sm:flex-row sm:items-end sm:justify-between">
        <h2 className="text-base font-semibold tracking-tight">{title}</h2>
        <p className="max-w-2xl text-xs text-muted-foreground">{description}</p>
      </div>
    </div>
  );
}

function PressureStrip({ view, summary }: { view?: FairshareLiveView; summary: FairshareSummary }) {
  const items = [
    {
      label: "In flight",
      value: view ? formatNumber(view.global_in_flight) : "--",
      sub: view && view.global_in_flight > 0 ? "requests active now" : "no active requests",
      tone: "neutral",
    },
    {
      label: "Queue",
      value: view ? formatNumber(view.global_queued) : "--",
      sub:
        view && view.global_queued > 0
          ? `${formatNumber(summary.waitingTenants)} tenants waiting`
          : "no backlog",
      tone: view && view.global_queued > 0 ? "warn" : "ok",
    },
    {
      label: "Tenants",
      value: view ? `${formatNumber(summary.activeTenants)} / ${formatNumber(view.tenants.length)}` : "--",
      sub: `${formatNumber(summary.starvedTenants)} below fair share`,
      tone: summary.starvedTenants > 0 ? "hot" : "ok",
    },
    {
      label: "Groups",
      value: view ? `${formatNumber(summary.activeGroups)} / ${formatNumber(view.groups.length)}` : "--",
      sub: `${formatNumber(summary.totalGroupWeight)} total group weight`,
      tone: "neutral",
    },
  ] satisfies MetricTile[];

  return (
    <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-4">
      {items.map((item) => (
        <MetricCard key={item.label} item={item} />
      ))}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Live timeline and scheduler now
// ---------------------------------------------------------------------------

function CapacityTimeline({
  history,
  groups,
  view,
}: {
  history: GroupHistoryPoint[];
  groups: GroupKey[];
  view?: FairshareLiveView;
}) {
  return (
    <Card className="h-full rounded-md">
      <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <CardTitle>Live occupancy</CardTitle>
          <CardDescription>Group activity over time; queued work uses the right axis</CardDescription>
        </div>
        {view && (
          <div className="flex flex-wrap justify-start gap-2 text-xs sm:justify-end">
            <Badge>{formatNumber(view.global_in_flight)} in flight</Badge>
            <Badge>{formatNumber(view.global_queued)} queued</Badge>
          </div>
        )}
      </CardHeader>
      <CardContent>
        {history.length === 0 ? (
          <EmptyState className="h-[28rem]">Waiting for live scheduler samples</EmptyState>
        ) : (
          <ChartShell heightClass="h-[28rem]">
            <ResponsiveContainer width="100%" height="100%">
              <ComposedChart data={history} margin={{ top: 8, right: 12, left: 4, bottom: 4 }}>
                <defs>
                  {groups.map((g, i) => (
                    <linearGradient key={g.key} id={`cap-${i}`} x1="0" y1="0" x2="0" y2="1">
                      <stop offset="0%" stopColor={g.color} stopOpacity={0.34} />
                      <stop offset="100%" stopColor={g.color} stopOpacity={0.05} />
                    </linearGradient>
                  ))}
                </defs>
                <CartesianGrid {...chartGrid} vertical={false} />
                <XAxis dataKey="time" tick={axisTick} axisLine={false} tickLine={false} minTickGap={36} />
                <YAxis
                  yAxisId="slots"
                  tick={axisTick}
                  axisLine={false}
                  tickLine={false}
                  width={52}
                  allowDecimals={false}
                  tickFormatter={compactAxis}
                  domain={[0, (max: number) => slotAxisMax(max)]}
                />
                <YAxis
                  yAxisId="queued"
                  orientation="right"
                  tick={{ ...axisTick, fill: QUEUED_COLOR }}
                  axisLine={false}
                  tickLine={false}
                  width={42}
                  allowDecimals={false}
                />
                <Tooltip cursor={timeCursor} content={tip()} />
                <Legend wrapperStyle={{ fontSize: 11 }} />
                {groups.map((g, i) => (
                  <Area
                    key={g.key}
                    yAxisId="slots"
                    type="monotone"
                    dataKey={g.key}
                    name={g.name}
                    stackId="slots"
                    stroke={g.color}
                    fill={`url(#cap-${i})`}
                    strokeWidth={1.25}
                    isAnimationActive={false}
                    dot={false}
                    activeDot={{ r: 3, strokeWidth: 0 }}
                  />
                ))}
                <Line
                  yAxisId="queued"
                  type="monotone"
                  dataKey="queued"
                  name="queued"
                  stroke={QUEUED_COLOR}
                  strokeWidth={1.5}
                  strokeDasharray="3 3"
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

function SchedulerNow({ view, summary }: { view?: FairshareLiveView; summary: FairshareSummary }) {
  const next = summary.nextTenant;
  const deficit = summary.largestDeficit;

  return (
    <Card className="h-full rounded-md">
      <CardHeader>
        <CardTitle>Scheduler now</CardTitle>
        <CardDescription>Admission pressure and current fair-share posture</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="grid grid-cols-2 gap-3">
          <MiniStat label="In flight" value={view ? formatNumber(view.global_in_flight) : "--"} />
          <MiniStat label="Queued" value={view ? formatNumber(view.global_queued) : "--"} tone={(view?.global_queued ?? 0) > 0 ? "warn" : "ok"} />
          <MiniStat label="Waiting tenants" value={view ? formatNumber(summary.waitingTenants) : "--"} tone={summary.waitingTenants > 0 ? "warn" : "ok"} />
          <MiniStat label="Below fair" value={view ? formatNumber(summary.starvedTenants) : "--"} tone={summary.starvedTenants > 0 ? "hot" : "ok"} />
        </div>

        <PriorityReadout
          title="Next waiting tenant"
          tenant={next}
          empty="No queued tenants"
          valueLabel={next ? `debt ${formatScore(tenantDebt(view, next))}` : ""}
        />
        <PriorityReadout
          title="Largest fair-share gap"
          tenant={deficit}
          empty="No active deficit"
          valueLabel={deficit ? formatDelta(fairnessGap(deficit)) : ""}
          highlight={deficit ? isWaitingBelowShare(deficit) : false}
        />
      </CardContent>
    </Card>
  );
}

function MiniStat({
  label,
  value,
  tone = "neutral",
}: {
  label: string;
  value: string;
  tone?: MetricTone;
}) {
  const toneClass = {
    ok: "text-[hsl(158_48%_56%)]",
    warn: "text-[hsl(38_75%_62%)]",
    hot: "text-[hsl(350_65%_64%)]",
    neutral: "text-foreground",
  }[tone];

  return (
    <div className="rounded-sm border border-border bg-background/35 px-3 py-2">
      <p className="text-[10px] uppercase tracking-wider text-muted-foreground">{label}</p>
      <p className={cn("mt-0.5 text-lg font-semibold tabular-nums", toneClass)}>{value}</p>
    </div>
  );
}

function PriorityReadout({
  title,
  tenant,
  empty,
  valueLabel,
  highlight,
}: {
  title: string;
  tenant?: TenantFairshareView;
  empty: string;
  valueLabel: string;
  highlight?: boolean;
}) {
  return (
    <div className="rounded-sm border border-border bg-background/35 px-3 py-3">
      <div className="flex items-center justify-between gap-3">
        <p className="text-[10px] uppercase tracking-wider text-muted-foreground">{title}</p>
        {tenant && (
          <span
            className={cn(
              "shrink-0 rounded-sm px-1.5 py-0.5 text-[10px] font-medium tabular-nums",
              highlight ? "bg-[hsl(350_65%_60%/0.16)] text-[hsl(350_65%_64%)]" : "bg-muted/45 text-muted-foreground",
            )}
          >
            {valueLabel}
          </span>
        )}
      </div>
      {tenant ? (
        <div className="mt-2 min-w-0">
          <p className="truncate text-sm font-medium">{tenant.name}</p>
          <p className="mt-0.5 truncate text-xs text-muted-foreground">
            {tenant.fairshare_group} / {formatNumber(tenant.in_flight)} active / {formatNumber(tenant.queued)} queued
          </p>
        </div>
      ) : (
        <p className="mt-2 text-sm text-muted-foreground">{empty}</p>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

function GroupAllocation({ view }: { view?: FairshareLiveView }) {
  const groups = useMemo(
    () =>
      [...(view?.groups ?? [])].sort((a, b) => {
        const pressure = b.in_flight + b.queued - (a.in_flight + a.queued);
        return pressure || b.weight_share - a.weight_share || a.name.localeCompare(b.name);
      }),
    [view],
  );

  return (
    <Card className="rounded-md">
      <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <CardTitle>Group allocation</CardTitle>
          <CardDescription>Caps, weights, entitlements, active slots, queued work, and served tokens</CardDescription>
        </div>
        {view && (
          <Badge className="capitalize">
            {view.algorithm} / {formatNumber(groups.length)} groups
          </Badge>
        )}
      </CardHeader>
      <CardContent>
        {groups.length === 0 ? (
          <EmptyState className="h-48">No group state in the live scheduler</EmptyState>
        ) : (
          <div className="space-y-3">
            {groups.map((g, i) => (
              <GroupAllocationRow key={g.name} group={g} index={i} maxInFlight={view?.max_in_flight ?? 0} />
            ))}
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function GroupAllocationRow({
  group,
  index,
  maxInFlight,
}: {
  group: GroupFairshareView;
  index: number;
  maxInFlight: number;
}) {
  const color = colorForGroup(group.name, index);
  const cap = Math.max(group.slot_cap, group.expected_slots, group.in_flight, 1);
  const activePct = clamp((group.in_flight / cap) * 100, 0, 100);
  const queuedPct = clamp((group.queued / Math.max(group.in_flight + group.queued, 1)) * 100, 0, 100);
  const expectedPct = clamp((group.expected_slots / Math.max(maxInFlight, 1)) * 100, 0, 100);

  return (
    <div className="rounded-md border border-border bg-background/25 px-4 py-3">
      <div className="grid gap-3 lg:grid-cols-[minmax(11rem,0.8fr)_minmax(0,1.6fr)_minmax(18rem,1fr)] lg:items-center">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <span className="h-2.5 w-2.5 shrink-0 rounded-full" style={{ background: color }} />
            <p className="truncate text-sm font-semibold">{group.name}</p>
          </div>
          <p className="mt-1 text-xs tabular-nums text-muted-foreground">
            w{formatNumber(group.weight)} / {formatPct(group.weight_share * 100)} share
          </p>
        </div>

        <div className="min-w-0">
          <div className="mb-1 flex items-center justify-between gap-2 text-[11px] tabular-nums text-muted-foreground">
            <span>{formatNumber(group.in_flight)} active</span>
            <span>{formatDecimal(group.expected_slots)} expected / cap {formatNumber(group.slot_cap)}</span>
          </div>
          <div className="relative h-3 overflow-hidden rounded-sm bg-muted/35">
            <div className="h-full rounded-sm" style={{ width: `${activePct}%`, background: color }} />
            <span
              className="absolute top-[-2px] h-[calc(100%+4px)] w-px bg-foreground/70"
              style={{ left: `${clamp((group.expected_slots / cap) * 100, 0, 100)}%` }}
            />
          </div>
          <div className="mt-1.5 h-1.5 overflow-hidden rounded-sm bg-muted/25">
            <div className="h-full rounded-sm bg-[hsl(38_65%_60%)]" style={{ width: `${queuedPct}%` }} />
          </div>
        </div>

        <div className="grid grid-cols-4 gap-2 text-right">
          <GroupNumber label="Queued" value={formatNumber(group.queued)} tone={group.queued > 0 ? "warn" : "neutral"} />
          <GroupNumber label="Served" value={formatCompact(group.served_tokens)} />
          <GroupNumber label="Debt" value={formatScore(group.share_score)} />
          <GroupNumber label="Global" value={formatPct(expectedPct)} />
        </div>
      </div>
    </div>
  );
}

function GroupNumber({
  label,
  value,
  tone = "neutral",
}: {
  label: string;
  value: string;
  tone?: "warn" | "neutral";
}) {
  return (
    <div className="min-w-0">
      <p className="text-[10px] uppercase tracking-wider text-muted-foreground">{label}</p>
      <p
        className={cn(
          "mt-0.5 truncate text-xs font-medium tabular-nums",
          tone === "warn" ? "text-[hsl(38_75%_62%)]" : "text-foreground",
        )}
      >
        {value}
      </p>
    </div>
  );
}

function ModelSlotPressure({ view, routes }: { view?: FairshareLiveView; routes: ModelRoute[] }) {
  const rows = useMemo(() => {
    const inFlight = view?.model_in_flight ?? {};
    const queued = view?.model_queued ?? {};
    const capByName = new Map(routes.map((r) => [r.model_name, r.max_in_flight ?? null]));
    const names = new Set<string>([...Object.keys(inFlight), ...Object.keys(queued)]);
    return [...names]
      .map((name) => ({
        name,
        inFlight: inFlight[name] ?? 0,
        queued: queued[name] ?? 0,
        cap: capByName.get(name) ?? null,
      }))
      .filter((r) => r.inFlight > 0 || r.queued > 0)
      .sort((a, b) => b.queued - a.queued || b.inFlight - a.inFlight);
  }, [view, routes]);

  return (
    <Card className="rounded-md">
      <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <CardTitle>Model slot pressure</CardTitle>
          <CardDescription>Live in-flight and queued requests against each model's slot cap.</CardDescription>
        </div>
        <div className="flex flex-wrap gap-2 text-[11px] text-muted-foreground">
          <span className="inline-flex items-center gap-1.5"><i className="h-2 w-2 rounded-sm bg-[hsl(205_55%_52%)]" />in-flight</span>
          <span className="inline-flex items-center gap-1.5"><i className="h-2 w-2 rounded-sm bg-[hsl(38_70%_52%)]" />queued</span>
        </div>
      </CardHeader>
      <CardContent>
        {rows.length === 0 ? (
          <EmptyState className="h-32">No models with active or queued requests</EmptyState>
        ) : (
          <div className="space-y-2.5">
            {rows.map((r) => {
              const cap = r.cap ?? Math.max(r.inFlight + r.queued, 1);
              const inflightPct = clamp((r.inFlight / cap) * 100, 0, 100);
              const queuedPct = clamp((r.queued / cap) * 100, 0, 100 - inflightPct);
              return (
                <div key={r.name} className="flex items-center gap-3 text-xs">
                  <span className="w-32 shrink-0 truncate font-medium" title={r.name}>{r.name}</span>
                  <div className="flex h-4 flex-1 overflow-hidden rounded-sm bg-background/40">
                    <div className="h-full bg-[hsl(205_55%_52%)] transition-all duration-500" style={{ width: `${inflightPct}%` }} />
                    <div className="h-full bg-[hsl(38_70%_52%)] transition-all duration-500" style={{ width: `${queuedPct}%` }} />
                  </div>
                  <span className="w-32 shrink-0 text-right font-mono text-[11px] tabular-nums text-muted-foreground">
                    {formatNumber(r.inFlight)} / {r.cap == null ? "∞" : formatNumber(r.cap)}
                    {r.queued > 0 ? ` · ${formatNumber(r.queued)} queued` : ""}
                  </span>
                </div>
              );
            })}
          </div>
        )}
      </CardContent>
    </Card>
  );
}

// ---------------------------------------------------------------------------
// Tenants
// ---------------------------------------------------------------------------

function FairshareBalance({ view }: { view?: FairshareLiveView }) {
  const rows = useMemo(() => {
    const tenants = (view?.tenants ?? [])
      .filter((t) => t.in_flight + t.queued > 0)
      .map((t) => ({ t, gap: fairnessGap(t), starved: isWaitingBelowShare(t) }))
      .sort((a, b) => a.gap - b.gap)
      .slice(0, 12);
    const maxAbs = tenants.reduce((m, r) => Math.max(m, Math.abs(r.gap), r.t.expected_slots), 1);
    return { tenants, maxAbs };
  }, [view]);

  return (
    <Card className="rounded-md">
      <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <CardTitle>Fair-share balance</CardTitle>
          <CardDescription>Active slots vs. fair entitlement. Left of center is below fair share.</CardDescription>
        </div>
        <div className="flex flex-wrap gap-2 text-[11px] text-muted-foreground">
          <span className="inline-flex items-center gap-1.5"><i className="h-2 w-2 rounded-sm" style={{ background: STARVED_COLOR }} />below fair</span>
          <span className="inline-flex items-center gap-1.5"><i className="h-2 w-2 rounded-sm bg-[hsl(160_45%_48%)]" />above fair</span>
        </div>
      </CardHeader>
      <CardContent>
        {rows.tenants.length === 0 ? (
          <EmptyState className="h-48">No active tenants contending right now</EmptyState>
        ) : (
          <div className="space-y-1.5">
            {rows.tenants.map(({ t, gap, starved }) => {
              const widthPct = clamp((Math.abs(gap) / rows.maxAbs) * 50, 0, 50);
              const color = starved ? STARVED_COLOR : "hsl(160 45% 48%)";
              return (
                <div key={t.tenant_id} className="flex items-center gap-3 text-xs">
                  <span className="w-32 shrink-0 truncate font-medium" title={t.name}>{t.name}</span>
                  <span className="w-16 shrink-0 truncate text-[11px] text-muted-foreground">{t.fairshare_group}</span>
                  <div className="relative h-4 flex-1 rounded-sm bg-background/40">
                    <span className="absolute left-1/2 top-[-2px] bottom-[-2px] w-px bg-foreground/40" />
                    <div
                      className="absolute top-0.5 bottom-0.5 rounded-sm transition-all duration-500"
                      style={gap >= 0
                        ? { left: "50%", width: `${widthPct}%`, background: color }
                        : { right: "50%", width: `${widthPct}%`, background: color }}
                    />
                  </div>
                  <span className="w-28 shrink-0 text-right font-mono text-[11px] tabular-nums" style={{ color: starved ? STARVED_COLOR : "hsl(240 6% 64%)" }}>
                    {formatDelta(gap)} / {formatDecimal(t.expected_slots)} exp
                  </span>
                </div>
              );
            })}
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function TenantOperations({ view }: { view?: FairshareLiveView }) {
  const [query, setQuery] = useState("");
  const [groupFilter, setGroupFilter] = useState("all");
  const [scope, setScope] = useState<TenantScope>("active");
  const [sort, setSort] = useState<TenantSort>("pressure");

  const groups = useMemo(() => {
    const names = new Set<string>();
    for (const g of view?.groups ?? []) names.add(g.name);
    for (const t of view?.tenants ?? []) names.add(t.fairshare_group);
    return [...names].sort((a, b) => a.localeCompare(b));
  }, [view]);

  const rows = useMemo(() => {
    const q = query.trim().toLowerCase();
    let out = [...(view?.tenants ?? [])];
    if (groupFilter !== "all") out = out.filter((t) => t.fairshare_group === groupFilter);
    if (scope === "active") out = out.filter(isTenantActive);
    if (scope === "waiting") out = out.filter((t) => t.queued > 0);
    if (scope === "starved") out = out.filter(isWaitingBelowShare);
    if (q) {
      out = out.filter(
        (t) =>
          t.name.toLowerCase().includes(q) ||
          t.tenant_id.toLowerCase().includes(q) ||
          t.fairshare_group.toLowerCase().includes(q),
      );
    }

    out.sort((a, b) => {
      switch (sort) {
        case "queued":
          return b.queued - a.queued || b.in_flight - a.in_flight;
        case "deficit":
          return fairnessGap(a) - fairnessGap(b) || b.queued - a.queued;
        case "served":
          return b.served_tokens - a.served_tokens;
        case "score":
          return tenantDebt(view, a) - tenantDebt(view, b);
        case "weight":
          return b.weight - a.weight;
        case "share":
          return b.weight_share - a.weight_share;
        case "pressure":
        default:
          return b.in_flight + b.queued - (a.in_flight + a.queued) || fairnessGap(a) - fairnessGap(b);
      }
    });

    return out;
  }, [view, query, groupFilter, scope, sort]);

  const shown = rows.slice(0, TENANT_PAGE);
  const starved = useMemo(() => rows.filter(isWaitingBelowShare).length, [rows]);
  const totalActive = view?.tenants.filter(isTenantActive).length ?? 0;
  const totalWaiting = view?.tenants.filter((t) => t.queued > 0).length ?? 0;
  const totalStarved = view?.tenants.filter(isWaitingBelowShare).length ?? 0;

  return (
    <Card className="rounded-md">
      <CardHeader className="gap-3 lg:flex-row lg:items-start lg:justify-between">
        <div>
          <CardTitle>Tenant contention</CardTitle>
          <CardDescription>
            {view ? `${formatNumber(rows.length)} tenant${rows.length === 1 ? "" : "s"}` : "Loading tenants"}
            {rows.length > TENANT_PAGE ? ` / showing ${formatNumber(TENANT_PAGE)}` : ""}
            {starved > 0 ? ` / ${formatNumber(starved)} starved` : ""}
          </CardDescription>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <div className="inline-flex rounded-sm border border-border bg-background/35 p-0.5">
            <ScopeToggle active={scope === "active"} label={`Active ${formatNumber(totalActive)}`} onClick={() => setScope("active")} />
            <ScopeToggle active={scope === "waiting"} label={`Waiting ${formatNumber(totalWaiting)}`} onClick={() => setScope("waiting")} />
            <ScopeToggle active={scope === "starved"} label={`Below fair ${formatNumber(totalStarved)}`} onClick={() => setScope("starved")} />
            <ScopeToggle active={scope === "all"} label="All" onClick={() => setScope("all")} />
          </div>
          <div className="relative">
            <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              type="search"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Search tenant, id, group"
              aria-label="Search tenants"
              className="h-8 w-60 pl-8 text-xs"
            />
          </div>
          <Select
            value={groupFilter}
            onChange={(e) => setGroupFilter(e.target.value)}
            aria-label="Filter tenants by group"
            className="h-8 w-40 text-xs"
          >
            <option value="all">All groups</option>
            {groups.map((g) => (
              <option key={g} value={g}>
                {g}
              </option>
            ))}
          </Select>
          <Select
            value={sort}
            onChange={(e) => setSort(e.target.value as TenantSort)}
            aria-label="Sort tenants"
            className="h-8 w-44 text-xs"
          >
            <option value="pressure">Active pressure</option>
            <option value="queued">Queued</option>
            <option value="deficit">Slot deficit</option>
            <option value="score">Scheduler debt</option>
            <option value="served">Served work</option>
            <option value="weight">Weight</option>
            <option value="share">Weight share</option>
          </Select>
        </div>
      </CardHeader>
      <CardContent className="p-0">
        <div className="overflow-x-auto">
          <table className="w-full min-w-[980px] text-sm">
            <thead>
              <tr className="border-b border-border text-left text-xs text-muted-foreground">
                <th className="px-6 py-3 font-medium">Tenant</th>
                <th className="px-3 py-3 font-medium">Group</th>
                <th className="px-3 py-3 text-right font-medium">Active</th>
                <th className="px-3 py-3 text-right font-medium">Queued</th>
                <th className="px-3 py-3 font-medium">Slots vs fair</th>
                <th className="px-3 py-3 text-right font-medium">Share</th>
                <th className="px-3 py-3 text-right font-medium">Debt</th>
                <th className="px-3 py-3 text-right font-medium">Served</th>
                <th className="px-6 py-3 text-right font-medium">Weight</th>
              </tr>
            </thead>
            <tbody>
              {shown.map((tenant) => (
                <TenantRow key={tenant.tenant_id} tenant={tenant} view={view} />
              ))}
              {shown.length === 0 && (
                <tr>
                  <td colSpan={9} className="px-6 py-12 text-center text-muted-foreground">
                    {view ? "No tenants match the current view" : "Waiting for tenant scheduler state"}
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      </CardContent>
    </Card>
  );
}

function TenantRow({ tenant, view }: { tenant: TenantFairshareView; view?: FairshareLiveView }) {
  const starved = isWaitingBelowShare(tenant);
  const color = colorForGroup(tenant.fairshare_group);

  return (
    <tr
      className={cn(
        "border-b border-border/60 transition-colors hover:bg-muted/20",
        starved && "bg-[hsl(350_65%_60%/0.07)]",
      )}
    >
      <td className="px-6 py-3">
        <div className="flex min-w-0 items-center gap-2">
          <span className="h-2.5 w-2.5 shrink-0 rounded-full" style={{ background: color }} />
          <div className="min-w-0">
            <div className="flex items-center gap-2">
              <p className="truncate font-medium">{tenant.name}</p>
              {starved && (
                <span className="rounded-sm bg-[hsl(350_65%_60%/0.16)] px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide text-[hsl(350_65%_64%)]">
                  starved
                </span>
              )}
            </div>
            <p className="mt-0.5 font-mono text-[10px] text-muted-foreground">{tenant.tenant_id.slice(0, 8)}</p>
          </div>
        </div>
      </td>
      <td className="px-3 py-3 text-muted-foreground">{tenant.fairshare_group}</td>
      <td className="px-3 py-3 text-right tabular-nums">{formatNumber(tenant.in_flight)}</td>
      <td className="px-3 py-3 text-right tabular-nums">
        <span className={tenant.queued > 0 ? "text-[hsl(38_75%_62%)]" : ""}>{formatNumber(tenant.queued)}</span>
      </td>
      <td className="px-3 py-3">
        <FairnessBar tenant={tenant} />
      </td>
      <td className="px-3 py-3 text-right tabular-nums text-muted-foreground">{formatPct(tenant.weight_share * 100)}</td>
      <td className="px-3 py-3 text-right font-mono text-xs tabular-nums text-muted-foreground">
        {formatScore(tenantDebt(view, tenant))}
      </td>
      <td className="px-3 py-3 text-right tabular-nums text-muted-foreground">{formatCompact(tenant.served_tokens)}</td>
      <td className="px-6 py-3">
        <WeightCell id={tenant.tenant_id} weight={tenant.weight} />
      </td>
    </tr>
  );
}

function FairnessBar({ tenant }: { tenant: TenantFairshareView }) {
  const entitled = Math.max(tenant.expected_slots, 0);
  const delta = fairnessGap(tenant);
  const color = fairnessColor(tenant);
  const scale = Math.max(entitled, tenant.in_flight, 1);
  const frac = clamp(Math.abs(delta) / scale, 0, 1);
  const widthPct = frac * 50;

  return (
    <div
      className="flex items-center gap-2"
      title={`${formatNumber(tenant.in_flight)} active vs ${formatDecimal(entitled)} expected slots`}
    >
      <div className="relative h-2.5 w-32 overflow-hidden rounded-sm bg-muted/40">
        <span className="absolute left-1/2 top-[-1px] h-[calc(100%+2px)] w-px -translate-x-1/2 bg-foreground/45" />
        <div
          className="absolute top-0 h-full transition-all duration-500"
          style={
            delta >= 0
              ? { left: "50%", width: `${widthPct}%`, background: color, borderRadius: "0 2px 2px 0" }
              : { right: "50%", width: `${widthPct}%`, background: color, borderRadius: "2px 0 0 2px" }
          }
        />
      </div>
      <span
        className="w-16 shrink-0 text-right font-mono text-[10px] tabular-nums"
        style={{ color: isWaitingBelowShare(tenant) ? STARVED_COLOR : "hsl(240 6% 64%)" }}
      >
        {formatDelta(delta)} / {formatDecimal(entitled)}
      </span>
    </div>
  );
}

function WeightCell({ id, weight }: { id: string; weight: number }) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(weight);
  const [pending, start] = useTransition();
  const queryClient = useQueryClient();

  const commit = () => {
    const next = Math.max(1, Math.round(draft) || 1);
    if (next === weight) {
      setEditing(false);
      return;
    }
    start(async () => {
      await setWeightAction(id, next);
      queryClient.invalidateQueries({ queryKey: ["fairshare-live"] });
      setEditing(false);
    });
  };

  if (!editing) {
    return (
      <button
        type="button"
        onClick={() => {
          setDraft(weight);
          setEditing(true);
        }}
        className="group ml-auto flex items-center justify-end gap-1.5 tabular-nums text-muted-foreground transition-colors hover:text-foreground"
        title="Edit tenant weight"
      >
        {formatNumber(weight)}
        <Pencil className="h-3 w-3 opacity-0 transition-opacity group-hover:opacity-70" />
      </button>
    );
  }

  return (
    <div className="flex items-center justify-end gap-1">
      <input
        type="number"
        min={1}
        autoFocus
        value={draft}
        disabled={pending}
        onChange={(e) => setDraft(Number(e.target.value))}
        onKeyDown={(e) => {
          if (e.key === "Enter") commit();
          if (e.key === "Escape") setEditing(false);
        }}
        aria-label="Fairshare weight"
        className="h-7 w-16 rounded-md border border-border bg-background px-2 text-right text-xs tabular-nums outline-none focus:ring-1 focus:ring-foreground/40"
      />
      <button
        type="button"
        onClick={commit}
        disabled={pending}
        aria-label="Apply weight"
        className="flex h-7 w-7 items-center justify-center rounded-md text-[hsl(158_48%_56%)] hover:bg-muted/40 disabled:opacity-50"
      >
        <Check className="h-3.5 w-3.5" />
      </button>
      <button
        type="button"
        onClick={() => setEditing(false)}
        disabled={pending}
        aria-label="Cancel"
        className="flex h-7 w-7 items-center justify-center rounded-md text-muted-foreground hover:bg-muted/40 disabled:opacity-50"
      >
        <X className="h-3.5 w-3.5" />
      </button>
    </div>
  );
}

function ScopeToggle({
  active,
  label,
  onClick,
}: {
  active: boolean;
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      aria-pressed={active}
      onClick={onClick}
      className={cn(
        "inline-flex h-7 items-center rounded-sm px-2.5 text-xs transition-colors",
        active ? "bg-muted text-foreground" : "text-muted-foreground hover:text-foreground",
      )}
    >
      {label}
    </button>
  );
}

// ---------------------------------------------------------------------------
// Throughput
// ---------------------------------------------------------------------------

function ThroughputPanel({
  data,
  series,
  metric,
  onMetricChange,
}: {
  data: Record<string, number | string>[];
  series: { key: string; name: string; color: string }[];
  metric: ThroughputMetric;
  onMetricChange: (metric: ThroughputMetric) => void;
}) {
  const valueName = metric === "requests" ? "Requests" : "Tokens";

  return (
    <Card className="h-full rounded-md">
      <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <CardTitle>Tenant throughput</CardTitle>
          <CardDescription>
            Last 30 minutes / 10s buckets / top {TOP_SERIES} tenants
          </CardDescription>
        </div>
        <div className="inline-flex rounded-sm border border-border bg-background/35 p-0.5">
          <MetricToggle
            active={metric === "requests"}
            onClick={() => onMetricChange("requests")}
            label="Requests"
          />
          <MetricToggle
            active={metric === "tokens"}
            onClick={() => onMetricChange("tokens")}
            label="Tokens"
          />
        </div>
      </CardHeader>
      <CardContent>
        {data.length === 0 ? (
          <EmptyState className="h-72">No tenant throughput in the selected window</EmptyState>
        ) : (
          <ChartShell heightClass="h-72">
            <ResponsiveContainer width="100%" height="100%">
              <AreaChart data={data} margin={{ top: 8, right: 12, left: 4, bottom: 4 }}>
                <defs>
                  {series.map((s, i) => (
                    <linearGradient key={s.key} id={`tp-${i}`} x1="0" y1="0" x2="0" y2="1">
                      <stop offset="0%" stopColor={s.color} stopOpacity={0.34} />
                      <stop offset="100%" stopColor={s.color} stopOpacity={0.05} />
                    </linearGradient>
                  ))}
                </defs>
                <CartesianGrid {...chartGrid} vertical={false} />
                <XAxis dataKey="time" tick={axisTick} axisLine={false} tickLine={false} minTickGap={42} />
                <YAxis
                  tick={axisTick}
                  axisLine={false}
                  tickLine={false}
                  width={46}
                  allowDecimals={false}
                  tickFormatter={metric === "tokens" ? compactAxis : undefined}
                />
                <Tooltip
                  cursor={timeCursor}
                  content={tip({
                    valueFormatter: metric === "tokens" ? compactAxis : undefined,
                  })}
                />
                <Legend wrapperStyle={{ fontSize: 11 }} />
                {series.map((s, i) => (
                  <Area
                    key={s.key}
                    type="monotone"
                    dataKey={s.key}
                    name={s.name}
                    stackId="throughput"
                    stroke={s.color}
                    fill={`url(#tp-${i})`}
                    strokeWidth={1.5}
                    isAnimationActive={false}
                    dot={false}
                    activeDot={{ r: 3, strokeWidth: 0 }}
                    connectNulls
                  />
                ))}
                <ReferenceLine y={0} stroke="hsl(240 4% 16%)" />
              </AreaChart>
            </ResponsiveContainer>
          </ChartShell>
        )}
        <p className="mt-3 text-xs text-muted-foreground">{valueName} refresh every {THROUGHPUT_POLL_MS / 1000}s</p>
      </CardContent>
    </Card>
  );
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

function slotAxisMax(max: number) {
  if (!Number.isFinite(max) || max <= 0) return 8;
  if (max <= 8) return 8;
  if (max <= 32) return Math.ceil(max * 1.35);
  if (max <= 256) return Math.ceil(max * 1.2);
  return Math.ceil(max * 1.12);
}

function groupDataKey(name: string) {
  return `group:${name}`;
}

function tenantLabel(id: string, tenantNames: Record<string, string>) {
  return tenantNames[id] ?? id.slice(0, 8);
}

function isTenantActive(t: TenantFairshareView) {
  return t.in_flight + t.queued > 0;
}

function fairnessGap(t: TenantFairshareView): number {
  return t.in_flight - t.expected_slots;
}

function fairnessColor(t: TenantFairshareView): string {
  if (isWaitingBelowShare(t)) return STARVED_COLOR;
  return fairnessGap(t) >= -0.5 ? HEALTHY_COLOR : UNDER_COLOR;
}

function tenantDebt(_view: FairshareLiveView | undefined, tenant: TenantFairshareView): number {
  return tenant.served_tokens / Math.max(tenant.weight, 1);
}

function pressureStatus(utilization: number, queued: number): MetricTone {
  if (queued > 0 && utilization >= 90) return "hot";
  if (queued > 0 || utilization >= 75) return "warn";
  return "ok";
}

