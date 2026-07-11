"use client";

import Link from "next/link";
import { memo, useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, RefreshCw, ShieldCheck } from "lucide-react";
import {
  Area,
  CartesianGrid,
  ComposedChart,
  Line,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import { axisTick, chartGrid, ChartShell, compactAxis, tip, timeCursor } from "@/components/chart-tooltip";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import {
  EmptyState,
  MetricCard,
  MetricToggle,
  StripMetric,
  type MetricTile,
  type MetricTone,
} from "@/components/dashboard-primitives";
import type { OverviewSummary } from "@/lib/overview-summary";
import { buildOverviewAttentionItems } from "@/lib/overview-attention";
import { isWaitingBelowShare } from "@/lib/fairshare";
import { formatCompact, formatDecimal } from "@/lib/format";
import type {
  FairshareLiveView,
  LiveStats,
  ModelHealthSummary,
  ModelRoute,
  TenantUsageTimePoint,
  UsageModelAgg,
  UsageTimePoint,
} from "@/lib/obleth";
import { cn, formatCurrency, formatNumber, getJson } from "@/lib/utils";
import { WINDOW_MS, type ModelWindow } from "@/components/model-metrics-detail";

const SUMMARY_POLL_MS = 30_000;
const FAST_POLL_MS = 2_000;
const STATS_POLL_MS = 5_000;
const USAGE_POLL_MS = 20_000;
const SLOW_POLL_MS = 60_000;

const HOUR_MS = 3_600_000;
const LIVE_BUCKET_MS = 60_000;

const TOP_MODELS = 8;

const TOKEN_COLOR = "hsl(205 18% 58%)";
const REQUEST_COLOR = "hsl(38 65% 62%)";

type TrafficRange = "live" | "day";

export function OverviewDashboard({
  models,
  initialSummary,
  initialVolumeSeries,
  initialTenantSeries,
  initialModelUsage,
  initialHealth,
  initialFairshare,
  initialStats,
}: {
  models: ModelRoute[];
  initialSummary: OverviewSummary;
  initialVolumeSeries: UsageTimePoint[];
  initialTenantSeries: TenantUsageTimePoint[];
  initialModelUsage: UsageModelAgg[];
  initialHealth: ModelHealthSummary[];
  initialFairshare?: FairshareLiveView;
  initialStats?: LiveStats;
}) {
  const queryClient = useQueryClient();
  const [trafficRange, setTrafficRange] = useState<TrafficRange>("live");
  const [modelWindow, setModelWindow] = useState<ModelWindow>("1h");

  const summaryQuery = useQuery({
    queryKey: ["live-summary"],
    queryFn: () => getJson<OverviewSummary>("/api/live/summary"),
    initialData: initialSummary,
    refetchInterval: SUMMARY_POLL_MS,
    staleTime: SUMMARY_POLL_MS / 2,
  });

  const statsQuery = useQuery({
    queryKey: ["gateway-stats"],
    queryFn: () => getJson<LiveStats>("/api/live/stats"),
    initialData: initialStats,
    refetchInterval: STATS_POLL_MS,
  });

  const fairshareQuery = useQuery({
    queryKey: ["fairshare-live"],
    queryFn: () => getJson<FairshareLiveView>("/api/live/fairshare"),
    initialData: initialFairshare,
    refetchInterval: FAST_POLL_MS,
  });

  const tenantSeriesQuery = useQuery({
    queryKey: ["overview-tenant-series"],
    queryFn: () =>
      getJson<TenantUsageTimePoint[]>(
        `/api/live/usage/tenants?bucket_ms=${LIVE_BUCKET_MS}&since_ms=${Date.now() - HOUR_MS}`,
      ),
    initialData: initialTenantSeries,
    refetchInterval: USAGE_POLL_MS,
  });

  const modelUsageQuery = useQuery({
    queryKey: ["usage-models-top", modelWindow],
    queryFn: () =>
      getJson<UsageModelAgg[]>(`/api/live/usage/models?since_ms=${Date.now() - WINDOW_MS[modelWindow]}`),
    initialData: initialModelUsage,
    refetchInterval: USAGE_POLL_MS,
  });

  const modelRoutesQuery = useQuery({
    queryKey: ["model-routes"],
    queryFn: () => getJson<ModelRoute[]>("/api/live/models"),
    initialData: models,
    refetchInterval: SLOW_POLL_MS,
  });

  const healthQuery = useQuery({
    queryKey: ["model-health"],
    queryFn: () => getJson<ModelHealthSummary[]>("/api/live/models/health"),
    initialData: initialHealth,
    refetchInterval: SLOW_POLL_MS,
  });

  const activeModels = modelRoutesQuery.data ?? models;
  const visibleModels = useMemo(() => activeModels.filter((model) => !isBenchmarkRoute(model)), [activeModels]);
  const activeHealth = healthQuery.data ?? initialHealth;
  const fairshare = fairshareQuery.data ?? initialFairshare;
  const stats = statsQuery.data ?? initialStats;

  const trafficSeries = useMemo(
    () => buildTrafficSeries(trafficRange, initialVolumeSeries, tenantSeriesQuery.data ?? []),
    [trafficRange, initialVolumeSeries, tenantSeriesQuery.data],
  );
  // Deliberately independent of the 2s fairshare tick: the top-models panel
  // only shows usage/health-derived fields, so its rows stay referentially
  // stable between usage polls and the memoized panel skips re-rendering.
  const modelRows = useMemo(
    () => buildModelRows(modelUsageQuery.data ?? [], visibleModels, activeHealth),
    [modelUsageQuery.data, visibleModels, activeHealth],
  );
  const capacity = useMemo(() => summarizeCapacity(fairshare, stats), [fairshare, stats]);
  const healthSummary = useMemo(() => summarizeHealth(activeHealth, visibleModels), [activeHealth, visibleModels]);
  const overviewStatus = useMemo(
    () => summarizeOverviewStatus(capacity, healthSummary, fairshare),
    [capacity, healthSummary, fairshare],
  );

  function refreshAll() {
    queryClient.invalidateQueries({ queryKey: ["live-summary"] });
    queryClient.invalidateQueries({ queryKey: ["gateway-stats"] });
    queryClient.invalidateQueries({ queryKey: ["fairshare-live"] });
    queryClient.invalidateQueries({ queryKey: ["overview-tenant-series"] });
    queryClient.invalidateQueries({ queryKey: ["usage-models-top"] });
    queryClient.invalidateQueries({ queryKey: ["model-routes"] });
    queryClient.invalidateQueries({ queryKey: ["model-health"] });
  }

  const isFetching =
    summaryQuery.isFetching ||
    statsQuery.isFetching ||
    fairshareQuery.isFetching ||
    tenantSeriesQuery.isFetching ||
    modelUsageQuery.isFetching;

  return (
    <div className="space-y-5">
      <OverviewConsoleHeader
        fairshare={fairshare}
        status={overviewStatus}
        isFetching={isFetching}
        onRefresh={refreshAll}
        updatedAt={Math.max(summaryQuery.dataUpdatedAt, statsQuery.dataUpdatedAt, healthQuery.dataUpdatedAt)}
      />

      <OverviewMetricStrip
        summary={summaryQuery.data}
        capacity={capacity}
        health={healthSummary}
      />

      <div className="grid gap-4 xl:grid-cols-[minmax(0,1.55fr)_minmax(21rem,0.75fr)]">
        <TrafficPanel
          series={trafficSeries}
          range={trafficRange}
          onRangeChange={setTrafficRange}
          updatedAt={tenantSeriesQuery.dataUpdatedAt}
        />
        <NeedsAttentionPanel
          capacity={capacity}
          health={healthSummary}
          status={overviewStatus}
          fairshare={fairshare}
        />
      </div>

      <TopModelsPanel rows={modelRows} activeWindow={modelWindow} onWindowChange={setModelWindow} />
    </div>
  );
}

function NeedsAttentionPanel({
  capacity,
  health,
  status,
  fairshare,
}: {
  capacity: CapacitySummary;
  health: HealthSummary;
  status: OverviewStatus;
  fairshare?: FairshareLiveView;
}) {
  const items = buildOverviewAttentionItems({
    unhealthyModels: health.unhealthy,
    unknownModels: health.unknown,
    queuedRequests: capacity.queued,
    waitingTenants: capacity.waitingTenants,
    starvedTenants: fairshare?.tenants.filter(isWaitingBelowShare).length ?? 0,
  });

  return (
    <Card className="h-full rounded-md">
      <CardHeader>
        <CardTitle>Needs attention</CardTitle>
        <CardDescription>{items.length > 0 ? "Prioritized conditions worth investigating now." : status.detail}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-2">
        {items.length === 0 ? (
          <div className="flex min-h-48 flex-col items-center justify-center rounded-sm border border-border bg-background/35 px-5 text-center">
            <ShieldCheck className="h-7 w-7 text-[hsl(160_28%_58%)]" />
            <p className="mt-3 text-sm font-medium">Everything looks clear</p>
            <p className="mt-1 max-w-xs text-xs text-muted-foreground">No model failures or admission backlog need attention.</p>
          </div>
        ) : items.map((item) => (
          <Link key={`${item.href}-${item.title}`} href={item.href} className="block rounded-sm border border-border bg-background/35 px-3 py-3 transition-colors hover:bg-muted/20">
            <div className="flex items-start gap-3">
              <AlertTriangle className={cn("mt-0.5 h-4 w-4 shrink-0", item.tone === "hot" ? "text-[hsl(350_55%_64%)]" : "text-[hsl(38_65%_62%)]")} />
              <div className="min-w-0">
                <p className="text-xs font-medium">{item.title}</p>
                <p className="mt-1 text-[11px] text-muted-foreground">{item.detail}</p>
              </div>
            </div>
          </Link>
        ))}
        <div className="grid grid-cols-2 gap-2 pt-2">
          <Button variant="outline" size="sm" asChild><Link href="/fairshare">Open Fairshare</Link></Button>
          <Button variant="outline" size="sm" asChild><Link href="/models">Open Models</Link></Button>
        </div>
      </CardContent>
    </Card>
  );
}

function OverviewConsoleHeader({
  fairshare,
  status,
  isFetching,
  onRefresh,
  updatedAt,
}: {
  fairshare?: FairshareLiveView;
  status: OverviewStatus;
  isFetching: boolean;
  onRefresh: () => void;
  updatedAt: number;
}) {
  return (
    <div className="rounded-md border border-border bg-card px-4 py-3">
      <div className="flex flex-col gap-3 md:flex-row md:items-center md:justify-between">
        <div className="min-w-0">
          <h1 className="text-lg font-semibold tracking-tight">Gateway overview</h1>
          <p className="mt-0.5 text-sm text-muted-foreground">Health, traffic, and the next action when something needs attention.</p>
          <div className="mt-3 flex flex-wrap items-center gap-2">
            <Badge className={cn("gap-1.5", status.tone === "hot" && "border-red-500/35 bg-red-500/10 text-red-300", status.tone === "warn" && "border-amber-500/35 bg-amber-500/10 text-amber-300")}>
              {status.tone === "ok" ? <ShieldCheck className="h-3.5 w-3.5" /> : <AlertTriangle className="h-3.5 w-3.5" />}
              {status.label}
            </Badge>
            <Badge className="capitalize">{fairshare?.algorithm ?? "loading"} admission</Badge>
            <Badge className="gap-1.5">
              <span className="h-1.5 w-1.5 rounded-full bg-[hsl(160_14%_58%)]" />
              live
            </Badge>
          </div>
          <p className="mt-2 truncate text-xs text-muted-foreground">
            {status.detail} / Updated {updatedAt ? new Date(updatedAt).toLocaleTimeString() : "--"}
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Button type="button" variant="secondary" size="sm" onClick={onRefresh}>
            <RefreshCw className={cn("h-3.5 w-3.5", isFetching && "animate-spin")} />
            Refresh
          </Button>
        </div>
      </div>
    </div>
  );
}

function OverviewMetricStrip({
  summary,
  capacity,
  health,
}: {
  summary: OverviewSummary;
  capacity: CapacitySummary;
  health: HealthSummary;
}) {
  const avgTokens = summary.requests > 0 ? Math.round(summary.tokens / summary.requests) : 0;
  const items = [
    {
      label: "Gateway health",
      value: health.unhealthy > 0 ? `${formatNumber(health.unhealthy)} unhealthy` : "Healthy",
      sub: `${formatNumber(health.healthy)} of ${formatNumber(health.enabled)} enabled routes healthy`,
      tone: health.unhealthy > 0 ? "hot" : health.unknown > 0 ? "warn" : "ok",
    },
    {
      label: "Live work",
      value: `${formatNumber(capacity.inFlight)} active`,
      sub: `${formatNumber(capacity.queued)} queued / ${formatNumber(capacity.waitingTenants)} tenants waiting`,
      tone: capacity.queued > 0 ? "warn" : "ok",
    },
    {
      label: "Traffic / 24h",
      value: `${formatCompact(summary.requests)} req`,
      sub: `${formatCompact(summary.tokens)} tokens / avg ${formatCompact(avgTokens)}`,
      tone: "neutral",
    },
    {
      label: "Cost / 24h",
      value: summary.hasPricing ? formatCurrency(summary.cost) : "--",
      sub: summary.hasPricing ? `${formatNumber(summary.activeTenants)} active tenants` : "Pricing is not configured",
      tone: "neutral",
    },
  ] satisfies MetricTile[];

  return (
    <div className="grid grid-cols-2 gap-3 xl:grid-cols-4">
      {items.map((item) => (
        <MetricCard key={item.label} item={item} />
      ))}
    </div>
  );
}

const TrafficPanel = memo(function TrafficPanel({
  series,
  range,
  onRangeChange,
  updatedAt,
}: {
  series: TrafficPoint[];
  range: TrafficRange;
  onRangeChange: (range: TrafficRange) => void;
  updatedAt: number;
}) {
  const totals = useMemo(
    () =>
      series.reduce(
        (acc, point) => ({ requests: acc.requests + point.requests, tokens: acc.tokens + point.tokens }),
        { requests: 0, tokens: 0 },
      ),
    [series],
  );

  return (
    <Card className="h-full rounded-md">
      <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <CardTitle>Traffic</CardTitle>
          <CardDescription>
            {range === "live" ? "Last 60 minutes / 1-minute buckets" : "Last 24 hours / 5-minute buckets"}
          </CardDescription>
        </div>
        <div className="flex items-center gap-3">
          <div className="hidden text-right text-xs tabular-nums text-muted-foreground sm:block">
            <p>{formatNumber(totals.requests)} requests</p>
            <p>{formatCompact(totals.tokens)} tokens</p>
          </div>
          <div className="inline-flex rounded-sm border border-border bg-background/40 p-0.5">
            <MetricToggle active={range === "live"} label="60m" onClick={() => onRangeChange("live")} />
            <MetricToggle active={range === "day"} label="24h" onClick={() => onRangeChange("day")} />
          </div>
        </div>
      </CardHeader>
      <CardContent>
        {series.length === 0 ? (
          <EmptyState className="h-[24rem]">No traffic in this window</EmptyState>
        ) : (
          <ChartShell heightClass="h-[24rem]">
            <ResponsiveContainer width="100%" height="100%">
              <ComposedChart data={series} margin={{ top: 8, right: 14, left: 4, bottom: 28 }}>
                <defs>
                  <linearGradient id="overview-tokens" x1="0" y1="0" x2="0" y2="1">
                    <stop offset="0%" stopColor={TOKEN_COLOR} stopOpacity={0.55} />
                    <stop offset="100%" stopColor={TOKEN_COLOR} stopOpacity={0.05} />
                  </linearGradient>
                </defs>
                <CartesianGrid {...chartGrid} vertical={false} />
                <XAxis dataKey="time" tick={axisTick} axisLine={false} tickLine={false} minTickGap={42} />
                <YAxis
                  yAxisId="tokens"
                  tick={axisTick}
                  axisLine={false}
                  tickLine={false}
                  width={46}
                  allowDecimals={false}
                  tickFormatter={compactAxis}
                />
                <YAxis
                  yAxisId="requests"
                  orientation="right"
                  tick={{ ...axisTick, fill: REQUEST_COLOR }}
                  axisLine={false}
                  tickLine={false}
                  width={38}
                  allowDecimals={false}
                />
                <Tooltip cursor={timeCursor} content={tip()} />
                <Area
                  yAxisId="tokens"
                  type="monotone"
                  dataKey="tokens"
                  name="Tokens"
                  stroke={TOKEN_COLOR}
                  fill="url(#overview-tokens)"
                  strokeWidth={1.5}
                  dot={false}
                  activeDot={{ r: 3, strokeWidth: 0 }}
                  isAnimationActive={false}
                />
                <Line
                  yAxisId="requests"
                  type="monotone"
                  dataKey="requests"
                  name="Requests"
                  stroke={REQUEST_COLOR}
                  strokeDasharray="4 3"
                  strokeWidth={1.5}
                  dot={false}
                  activeDot={{ r: 3, strokeWidth: 0 }}
                  isAnimationActive={false}
                />
              </ComposedChart>
            </ResponsiveContainer>
          </ChartShell>
        )}
        <p className="mt-3 text-xs text-muted-foreground">
          {range === "live" && updatedAt ? `Updated ${new Date(updatedAt).toLocaleTimeString()}` : "24h snapshot"}
        </p>
      </CardContent>
    </Card>
  );
});

const TopModelsPanel = memo(function TopModelsPanel({
  rows,
  activeWindow,
  onWindowChange,
}: {
  rows: ModelDisplayRow[];
  activeWindow: ModelWindow;
  onWindowChange: (window: ModelWindow) => void;
}) {
  const top = useMemo(() => [...rows].sort((a, b) => b.requests - a.requests).slice(0, TOP_MODELS), [rows]);
  return (
    <Card className="rounded-md">
      <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <CardTitle>Top models</CardTitle>
          <CardDescription>Highest request volume with the latency signals needed for quick triage.</CardDescription>
        </div>
        <div className="inline-flex rounded-sm border border-border bg-background/40 p-0.5">
          {(Object.keys(WINDOW_MS) as ModelWindow[]).map((window) => (
            <MetricToggle key={window} active={activeWindow === window} label={window} onClick={() => onWindowChange(window)} />
          ))}
        </div>
      </CardHeader>
      <CardContent>
        {top.length === 0 ? <EmptyState className="h-48">No model traffic in this window</EmptyState> : (
          <div className="divide-y divide-border overflow-hidden rounded-sm border border-border">
            {top.map((row) => (
              <Link key={row.model} href={`/models?model=${encodeURIComponent(row.model)}`} className="grid grid-cols-[minmax(0,1fr)_repeat(3,auto)] items-center gap-4 bg-background/25 px-3 py-2.5 transition-colors hover:bg-muted/20">
                <div className="min-w-0"><p className="truncate text-xs font-medium">{row.model}</p><p className="mt-0.5 text-[11px] capitalize text-muted-foreground">{row.status}</p></div>
                <StripMetric label="requests" value={formatNumber(row.requests)} />
                <StripMetric label="gen tok/s" value={formatDecimal(row.genTps)} />
                <StripMetric label="TTFB" value={`${formatNumber(Math.round(row.p50TtftMs))}ms`} tone={row.p50TtftMs > 1000 ? "warn" : "neutral"} />
              </Link>
            ))}
          </div>
        )}
        <div className="mt-3 flex flex-wrap gap-4 text-xs text-muted-foreground">
          <Link href="/models" className="underline underline-offset-2 hover:text-foreground">View all models</Link>
          <Link href="/logs" className="underline underline-offset-2 hover:text-foreground">Open request logs</Link>
          <Link href="/reports" className="underline underline-offset-2 hover:text-foreground">Open reports</Link>
        </div>
      </CardContent>
    </Card>
  );
});

interface CapacitySummary {
  inFlight: number;
  queued: number;
  max: number;
  headroom: number;
  utilization: number;
  waitingTenants: number;
  tone: MetricTone;
}

interface OverviewStatus {
  tone: MetricTone;
  label: string;
  detail: string;
}

function summarizeOverviewStatus(
  capacity: CapacitySummary,
  health: HealthSummary,
  fairshare?: FairshareLiveView,
): OverviewStatus {
  const issues = [
    capacity.queued > 0,
    health.unhealthy > 0,
    health.unknown > 0,
    (fairshare?.tenants ?? []).some(isWaitingBelowShare),
  ].filter(Boolean).length;
  const waitingBelowShare = fairshare?.tenants.filter(isWaitingBelowShare).length ?? 0;

  if (health.unhealthy > 0 || (capacity.queued > 0 && capacity.utilization >= 90)) {
    return {
      tone: "hot",
      label: `${formatNumber(issues)} need attention`,
      detail:
        health.unhealthy > 0
          ? `${formatNumber(health.unhealthy)} model route${health.unhealthy === 1 ? "" : "s"} unhealthy`
          : `${formatNumber(capacity.queued)} queued while the scheduler is saturated`,
    };
  }

  if (capacity.queued > 0 || health.unknown > 0 || waitingBelowShare > 0) {
    return {
      tone: "warn",
      label: `${formatNumber(Math.max(1, issues))} watch item${issues === 1 ? "" : "s"}`,
      detail:
        capacity.queued > 0
          ? `${formatNumber(capacity.waitingTenants)} waiting tenant${capacity.waitingTenants === 1 ? "" : "s"} across ${formatNumber(capacity.queued)} queued requests`
          : waitingBelowShare > 0
            ? `${formatNumber(waitingBelowShare)} tenant${waitingBelowShare === 1 ? "" : "s"} below fair share`
            : `${formatNumber(health.unknown)} route${health.unknown === 1 ? "" : "s"} have unknown health`,
    };
  }

  return {
    tone: "ok",
    label: "All clear",
    detail: "No backlog, routes healthy",
  };
}

function summarizeCapacity(view?: FairshareLiveView, stats?: LiveStats): CapacitySummary {
  const inFlight = view?.global_in_flight ?? stats?.in_flight ?? 0;
  const queued = view?.global_queued ?? stats?.queued ?? 0;
  const max = view?.max_in_flight ?? stats?.max_in_flight ?? 0;
  const utilization = max > 0 ? (inFlight / max) * 100 : 0;
  const waitingTenants = view?.tenants.filter((tenant) => tenant.queued > 0).length ?? 0;
  return {
    inFlight,
    queued,
    max,
    headroom: Math.max(0, max - inFlight),
    utilization,
    waitingTenants,
    tone: queued > 0 && utilization >= 90 ? "hot" : queued > 0 || utilization >= 75 ? "warn" : "ok",
  };
}

interface HealthSummary {
  total: number;
  enabled: number;
  healthy: number;
  unhealthy: number;
  maintenance: number;
  unknown: number;
}

function summarizeHealth(health: ModelHealthSummary[], models: ModelRoute[]): HealthSummary {
  const byId = new Map(health.map((row) => [row.model_id, row]));
  const summary: HealthSummary = {
    total: models.length,
    enabled: models.filter((model) => model.enabled).length,
    healthy: 0,
    unhealthy: 0,
    maintenance: 0,
    unknown: 0,
  };

  for (const model of models) {
    if (!model.enabled) continue;
    const row = byId.get(model.id);
    const status = row ? healthStatus(row) : "unknown";
    if (status === "healthy") summary.healthy += 1;
    else if (status === "unhealthy") summary.unhealthy += 1;
    else if (status === "maintenance") summary.maintenance += 1;
    else summary.unknown += 1;
  }

  return summary;
}

interface TrafficPoint {
  time: string;
  requests: number;
  tokens: number;
}

function buildTrafficSeries(range: TrafficRange, volumeSeries: UsageTimePoint[], tenantSeries: TenantUsageTimePoint[]): TrafficPoint[] {
  if (range === "day") {
    return volumeSeries
      .slice()
      .sort((a, b) => a.bucket_ms - b.bucket_ms)
      .map((point) => ({
        time: new Date(point.bucket_ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }),
        requests: Number(point.requests),
        tokens: Number(point.total_tokens),
      }));
  }

  const buckets = new Map<number, { requests: number; tokens: number }>();
  for (const point of tenantSeries) {
    const current = buckets.get(point.bucket_ms) ?? { requests: 0, tokens: 0 };
    current.requests += Number(point.requests);
    current.tokens += Number(point.total_tokens);
    buckets.set(point.bucket_ms, current);
  }

  return [...buckets.entries()]
    .sort((a, b) => a[0] - b[0])
    .map(([bucketMs, point]) => ({
      time: new Date(bucketMs).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }),
      requests: point.requests,
      tokens: point.tokens,
    }));
}

interface ModelDisplayRow {
  model: string;
  requests: number;
  tokens: number;
  genTps: number;
  p50TtftMs: number;
  status: string;
}

function buildModelRows(
  usage: UsageModelAgg[],
  routes: ModelRoute[],
  health: ModelHealthSummary[],
): ModelDisplayRow[] {
  const routeByName = new Map(routes.map((route) => [route.model_name, route]));
  const healthById = new Map(health.map((row) => [row.model_id, row]));

  return usage
    .filter((row) => !isBenchmarkModelName(row.model))
    .map((row) => {
      const route = routeByName.get(row.model);
      const summary = route ? healthById.get(route.id) : undefined;
      return {
        model: row.model,
        requests: Number(row.requests),
        tokens: Number(row.total_tokens),
        genTps: Number(row.gen_tokens_per_sec),
        p50TtftMs: Number(row.p50_ttft_ms),
        status: route && !route.enabled ? "disabled" : summary ? healthStatus(summary) : route ? "unknown" : "unrouted",
      };
    })
    .filter((row) => row.requests > 0)
    .sort((a, b) => b.requests - a.requests || b.tokens - a.tokens);
}

function healthStatus(row: ModelHealthSummary) {
  if (row.maintenance_until && new Date(row.maintenance_until).getTime() > Date.now()) return "maintenance";
  return row.status || "unknown";
}

function isBenchmarkRoute(model: ModelRoute) {
  return isBenchmarkModelName([model.model_name, model.upstream_model, model.api_base].join(" "));
}

function isBenchmarkModelName(value: string) {
  const s = value.toLowerCase();
  return s.includes("benchmark") || s.includes("mock-model") || s.includes("mock-backend");
}
