"use client";

import Link from "next/link";
import { useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, Boxes, Gauge, KeyRound, Radio, RefreshCw, ShieldCheck } from "lucide-react";
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
import type { OverviewSummary } from "@/lib/overview-summary";
import type {
  CacheStats,
  FairshareLiveView,
  LiveStats,
  ModelHealthSummary,
  ModelRoute,
  Tenant,
  TenantUsageTimePoint,
  UsageAgg,
  UsageKeyAgg,
  UsageLogEntry,
  UsageModelAgg,
  UsageTimePoint,
} from "@/lib/obleth";
import { cn, formatCurrency, formatNumber } from "@/lib/utils";
import { WINDOW_MS, type ModelWindow } from "@/components/model-metrics-detail";

const SUMMARY_POLL_MS = 30_000;
const FAST_POLL_MS = 2_000;
const STATS_POLL_MS = 5_000;
const USAGE_POLL_MS = 20_000;
const REQUEST_LOG_POLL_MS = 15_000;
const SLOW_POLL_MS = 60_000;

const DAY_MS = 86_400_000;
const HOUR_MS = 3_600_000;
const LIVE_BUCKET_MS = 60_000;

const TOP_TENANTS = 8;
const TOP_MODELS = 8;
const TOP_KEYS = 10;
const LIVE_REQUEST_ROWS = 12;

const PALETTE = [
  "hsl(210 8% 70%)",
  "hsl(205 13% 58%)",
  "hsl(165 11% 56%)",
  "hsl(35 12% 58%)",
  "hsl(260 8% 62%)",
  "hsl(190 9% 56%)",
  "hsl(350 22% 62%)",
];

const GROUP_PALETTE: Record<string, string> = {
  chatbot: "hsl(160 13% 58%)",
  api: "hsl(205 13% 62%)",
  analytics: "hsl(35 13% 58%)",
  batch: "hsl(260 9% 62%)",
  default: "hsl(240 6% 62%)",
};

const TOKEN_COLOR = "hsl(205 18% 58%)";
const REQUEST_COLOR = "hsl(38 65% 62%)";

type TrafficRange = "live" | "day";
type MetricTone = "ok" | "warn" | "hot" | "neutral";
type ActivityView = "tenants" | "models" | "keys";
type ModelSort = "requests" | "genTps" | "aggTps" | "ttft" | "e2e" | "tokens" | "users";

export function OverviewDashboard({
  tenants,
  models,
  initialSummary,
  initialVolumeSeries,
  initialTenantUsage,
  initialTenantSeries,
  initialModelUsage,
  initialKeyUsage,
  initialCacheStats,
  initialHealth,
  initialFairshare,
  initialStats,
}: {
  tenants: Tenant[];
  models: ModelRoute[];
  initialSummary: OverviewSummary;
  initialVolumeSeries: UsageTimePoint[];
  initialTenantUsage: UsageAgg[];
  initialTenantSeries: TenantUsageTimePoint[];
  initialModelUsage: UsageModelAgg[];
  initialKeyUsage: UsageKeyAgg[];
  initialCacheStats?: CacheStats;
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

  const keyUsageQuery = useQuery({
    queryKey: ["usage-keys-top"],
    queryFn: () => getJson<UsageKeyAgg[]>(`/api/live/usage/keys?since_ms=${Date.now() - HOUR_MS}&limit=${TOP_KEYS}`),
    initialData: initialKeyUsage,
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

  const cacheQuery = useQuery({
    queryKey: ["cache-stats"],
    queryFn: () => getJson<CacheStats>(`/api/live/cache?since_ms=${Date.now() - DAY_MS}`),
    initialData: initialCacheStats,
    refetchInterval: SLOW_POLL_MS,
  });

  const tenantNames = useMemo(() => Object.fromEntries(tenants.map((t) => [t.id, t.name])), [tenants]);
  const tenantGroups = useMemo(() => Object.fromEntries(tenants.map((t) => [t.id, t.fairshare_group])), [tenants]);
  const activeModels = modelRoutesQuery.data ?? models;
  const visibleModels = useMemo(() => activeModels.filter((model) => !isBenchmarkRoute(model)), [activeModels]);
  const activeHealth = healthQuery.data ?? initialHealth;
  const activeCache = cacheQuery.data ?? initialCacheStats;
  const fairshare = fairshareQuery.data ?? initialFairshare;
  const stats = statsQuery.data ?? initialStats;

  const trafficSeries = useMemo(
    () => buildTrafficSeries(trafficRange, initialVolumeSeries, tenantSeriesQuery.data ?? []),
    [trafficRange, initialVolumeSeries, tenantSeriesQuery.data],
  );
  const tenantRows = useMemo(
    () => buildTenantRows(tenantSeriesQuery.data ?? [], initialTenantUsage, tenants, fairshare),
    [tenantSeriesQuery.data, initialTenantUsage, tenants, fairshare],
  );
  const modelRows = useMemo(
    () => buildModelRows(modelUsageQuery.data ?? [], visibleModels, activeHealth, fairshare),
    [modelUsageQuery.data, visibleModels, activeHealth, fairshare],
  );
  const keyRows = useMemo(
    () => buildKeyRows(keyUsageQuery.data ?? [], tenantNames, tenantGroups),
    [keyUsageQuery.data, tenantNames, tenantGroups],
  );

  const capacity = summarizeCapacity(fairshare, stats);
  const healthSummary = summarizeHealth(activeHealth, visibleModels);
  const cacheSummary = summarizeCache(activeCache);
  const overviewStatus = summarizeOverviewStatus(capacity, healthSummary, fairshare);

  function refreshAll() {
    queryClient.invalidateQueries({ queryKey: ["live-summary"] });
    queryClient.invalidateQueries({ queryKey: ["gateway-stats"] });
    queryClient.invalidateQueries({ queryKey: ["fairshare-live"] });
    queryClient.invalidateQueries({ queryKey: ["overview-tenant-series"] });
    queryClient.invalidateQueries({ queryKey: ["usage-models-top"] });
    queryClient.invalidateQueries({ queryKey: ["usage-keys-top"] });
    queryClient.invalidateQueries({ queryKey: ["model-routes"] });
    queryClient.invalidateQueries({ queryKey: ["model-health"] });
    queryClient.invalidateQueries({ queryKey: ["cache-stats"] });
  }

  const isFetching =
    summaryQuery.isFetching ||
    statsQuery.isFetching ||
    fairshareQuery.isFetching ||
    tenantSeriesQuery.isFetching ||
    modelUsageQuery.isFetching ||
    keyUsageQuery.isFetching;

  return (
    <div className="space-y-5">
      <OverviewConsoleHeader
        fairshare={fairshare}
        status={overviewStatus}
        isFetching={isFetching}
        onRefresh={refreshAll}
      />

      <OverviewMetricStrip
        summary={summaryQuery.data}
        capacity={capacity}
        health={healthSummary}
        cache={cacheSummary}
        tenants={tenants}
      />

      <div className="grid gap-4 xl:grid-cols-[minmax(0,1.55fr)_minmax(21rem,0.75fr)]">
        <TrafficPanel
          series={trafficSeries}
          range={trafficRange}
          onRangeChange={setTrafficRange}
          updatedAt={tenantSeriesQuery.dataUpdatedAt}
        />
        <OperationsPanel
          summary={summaryQuery.data}
          capacity={capacity}
          health={healthSummary}
          cache={cacheSummary}
          modelCount={visibleModels.length}
          status={overviewStatus}
        />
      </div>

      <div className="space-y-4">
        <MetricsExplorer
          tenantRows={tenantRows}
          modelRows={modelRows}
          keyRows={keyRows}
          activeWindow={modelWindow}
          onWindowChange={setModelWindow}
        />
        <RequestFeedPanel />
      </div>
    </div>
  );
}

function OverviewConsoleHeader({
  fairshare,
  status,
  isFetching,
  onRefresh,
}: {
  fairshare?: FairshareLiveView;
  status: OverviewStatus;
  isFetching: boolean;
  onRefresh: () => void;
}) {
  return (
    <div className="rounded-md border border-border bg-card px-4 py-3">
      <div className="flex flex-col gap-3 md:flex-row md:items-center md:justify-between">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
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
          <p className="mt-2 truncate text-xs text-muted-foreground">{status.detail}</p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Button type="button" variant="outline" size="sm" asChild>
            <Link href="/fairshare">
              <Gauge className="h-3.5 w-3.5" />
              Fairshare
            </Link>
          </Button>
          <Button type="button" variant="outline" size="sm" asChild>
            <Link href="/models">
              <Boxes className="h-3.5 w-3.5" />
              Models
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

function OverviewMetricStrip({
  summary,
  capacity,
  health,
  cache,
  tenants,
}: {
  summary: OverviewSummary;
  capacity: CapacitySummary;
  health: HealthSummary;
  cache: CacheSummary;
  tenants: Tenant[];
}) {
  const avgTokens = summary.requests > 0 ? Math.round(summary.tokens / summary.requests) : 0;
  const activeGroups = new Set(tenants.filter((t) => t.fairshare_group).map((t) => t.fairshare_group)).size;
  const items = [
    {
      label: "Backlog",
      value: formatNumber(capacity.queued),
      sub: capacity.waitingTenants > 0 ? `${formatNumber(capacity.waitingTenants)} tenants waiting` : "no backlog",
      tone: capacity.queued > 0 ? "warn" : "ok",
    },
    {
      label: "Traffic 24h",
      value: `${formatCompact(summary.requests)} req`,
      sub: `${formatCompact(summary.tokens)} tokens / avg ${formatCompact(avgTokens)}`,
      tone: "neutral",
    },
    {
      label: "Routes",
      value: `${formatNumber(health.healthy)} / ${formatNumber(health.enabled)}`,
      sub: health.unhealthy > 0 ? `${formatNumber(health.unhealthy)} unhealthy` : `${formatNumber(health.unknown)} unknown`,
      tone: health.unhealthy > 0 ? "hot" : health.unknown > 0 ? "warn" : "ok",
    },
    {
      label: "Cost and cache",
      value: summary.hasPricing ? formatCurrency(summary.cost) : "--",
      sub: `${formatNumber(summary.activeTenants)} active tenants / cache ${cache.hitRateLabel} / ${formatNumber(activeGroups)} groups`,
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

interface MetricTile {
  label: string;
  value: string;
  sub: string;
  tone: MetricTone;
}

function MetricCard({ item }: { item: MetricTile }) {
  const tone = {
    ok: "text-foreground",
    warn: "text-[hsl(38_65%_62%)]",
    hot: "text-[hsl(350_55%_64%)]",
    neutral: "text-foreground",
  }[item.tone];

  return (
    <div className="rounded-md border border-border bg-card/55 px-4 py-3">
      <p className="text-[10px] uppercase tracking-wider text-muted-foreground">{item.label}</p>
      <p className={cn("mt-1 truncate text-xl font-semibold tabular-nums", tone)}>{item.value}</p>
      <p className="mt-0.5 truncate text-[11px] tabular-nums text-muted-foreground/75">{item.sub}</p>
    </div>
  );
}

function TrafficPanel({
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
  const totals = series.reduce(
    (acc, point) => ({ requests: acc.requests + point.requests, tokens: acc.tokens + point.tokens }),
    { requests: 0, tokens: 0 },
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
}

function OperationsPanel({
  summary,
  capacity,
  health,
  cache,
  modelCount,
  status,
}: {
  summary: OverviewSummary;
  capacity: CapacitySummary;
  health: HealthSummary;
  cache: CacheSummary;
  modelCount: number;
  status: OverviewStatus;
}) {
  const checks = [
    {
      label: "Admission",
      value: capacity.queued > 0 ? `${formatNumber(capacity.queued)} queued` : "clear",
      detail:
        capacity.waitingTenants > 0
          ? `${formatNumber(capacity.waitingTenants)} tenant${capacity.waitingTenants === 1 ? "" : "s"} waiting`
          : "no tenants waiting",
      tone: capacity.queued > 0 ? "warn" : "ok",
      href: "/fairshare",
    },
    {
      label: "Models",
      value: health.unhealthy > 0 ? `${formatNumber(health.unhealthy)} unhealthy` : "healthy",
      detail: `${formatNumber(health.healthy)} healthy / ${formatNumber(health.enabled)} enabled`,
      tone: health.unhealthy > 0 ? "hot" : health.unknown > 0 ? "warn" : "ok",
      href: "/models",
    },
    {
      label: "Tenants",
      value: `${formatNumber(summary.activeTenants)} active`,
      detail: `${formatNumber(summary.tenantCount)} total / ${formatNumber(summary.keyCount)} keys`,
      tone: "neutral",
      href: "/tenants",
    },
    {
      label: "Cache",
      value: cache.hitRateLabel,
      detail: cache.tokensSaved > 0 ? `${formatCompact(cache.tokensSaved)} tokens saved` : "no savings window",
      tone: "neutral",
      href: "/settings",
    },
  ] satisfies SnapshotCheck[];

  return (
    <Card className="h-full rounded-md">
      <CardHeader>
        <CardTitle>Gateway snapshot</CardTitle>
        <CardDescription>{status.detail}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="rounded-sm border border-border bg-background/35 p-3">
          <div className="mb-3 flex items-center justify-between gap-3">
            <div>
              <p className="text-xs font-medium">Live scheduler</p>
              <p className="mt-0.5 text-[11px] text-muted-foreground">Current admission pressure</p>
            </div>
            <Badge className={capacity.queued > 0 ? "border-amber-500/35 bg-amber-500/10 text-amber-300" : ""}>
              {capacity.queued > 0 ? "waiting" : "clear"}
            </Badge>
          </div>
          <div className="mt-2 grid grid-cols-3 gap-2 text-xs">
            <DetailStat label="In flight" value={formatNumber(capacity.inFlight)} />
            <DetailStat label="Queued" value={formatNumber(capacity.queued)} tone={capacity.queued > 0 ? "warn" : "neutral"} />
            <DetailStat label="Waiting" value={formatNumber(capacity.waitingTenants)} tone={capacity.waitingTenants > 0 ? "warn" : "neutral"} />
          </div>
        </div>

        <div className="grid grid-cols-2 gap-2 text-xs">
          <DetailStat label="Tenants" value={`${formatNumber(summary.activeTenants)} / ${formatNumber(summary.tenantCount)}`} />
          <DetailStat label="Routes" value={`${formatNumber(health.enabled)} / ${formatNumber(modelCount)}`} />
          <DetailStat label="Keys" value={formatNumber(summary.keyCount)} />
          <DetailStat label="Cache saved" value={formatCompact(cache.tokensSaved)} />
        </div>

        <div className="space-y-2">
          {checks.map((check) => (
            <SnapshotCheckRow key={check.label} check={check} />
          ))}
        </div>

        <div className="rounded-sm border border-border bg-background/35 p-3">
          <div className="flex items-start justify-between gap-3">
            <div className="min-w-0">
              <p className="text-xs font-medium">Model health</p>
              <p className="mt-0.5 truncate text-[11px] text-muted-foreground">
                {formatNumber(health.healthy)} healthy / {formatNumber(health.maintenance)} maintenance / {formatNumber(health.unknown)} unknown
              </p>
            </div>
            {health.unhealthy > 0 ? (
              <Badge className="gap-1.5 border-red-500/35 bg-red-500/10 text-red-300">
                <AlertTriangle className="h-3.5 w-3.5" />
                {formatNumber(health.unhealthy)}
              </Badge>
            ) : (
              <Badge>ok</Badge>
            )}
          </div>
        </div>

        <div className="grid grid-cols-2 gap-2">
          <Button type="button" variant="outline" size="sm" asChild>
            <Link href="/fairshare">
              <Gauge className="h-3.5 w-3.5" />
              Fairshare
            </Link>
          </Button>
          <Button type="button" variant="outline" size="sm" asChild>
            <Link href="/models">
              <Boxes className="h-3.5 w-3.5" />
              Models
            </Link>
          </Button>
        </div>
      </CardContent>
    </Card>
  );
}

interface SnapshotCheck {
  label: string;
  value: string;
  detail: string;
  tone: MetricTone;
  href: string;
}

function SnapshotCheckRow({ check }: { check: SnapshotCheck }) {
  const toneClass =
    check.tone === "hot"
      ? "text-[hsl(350_55%_64%)]"
      : check.tone === "warn"
        ? "text-[hsl(38_65%_62%)]"
        : "text-foreground";

  return (
    <Link
      href={check.href}
      className="block rounded-sm border border-border bg-background/35 px-3 py-2 transition-colors hover:bg-muted/20"
    >
      <div className="flex items-center justify-between gap-3">
        <div className="min-w-0">
          <p className="truncate text-xs font-medium">{check.label}</p>
          <p className="mt-0.5 truncate text-[11px] text-muted-foreground">{check.detail}</p>
        </div>
        <span className={cn("shrink-0 text-xs font-medium tabular-nums", toneClass)}>{check.value}</span>
      </div>
    </Link>
  );
}

function MetricsExplorer({
  tenantRows,
  modelRows,
  keyRows,
  activeWindow,
  onWindowChange,
}: {
  tenantRows: TenantDisplayRow[];
  modelRows: ModelDisplayRow[];
  keyRows: KeyDisplayRow[];
  activeWindow: ModelWindow;
  onWindowChange: (w: ModelWindow) => void;
}) {
  const [view, setView] = useState<ActivityView>("models");
  const [sort, setSort] = useState<ModelSort>("requests");
  const [expanded, setExpanded] = useState<string | null>(null);
  const [tenantSort, setTenantSort] = useState<"requests" | "queued" | "tokens">("requests");
  const [keySort, setKeySort] = useState<"requests" | "tokens">("requests");

  const sortedModels = useMemo(() => sortModelRows(modelRows, sort), [modelRows, sort]);

  const sortedTenants = useMemo(() => {
    const c = [...tenantRows];
    if (tenantSort === "queued") c.sort((a, b) => b.queued - a.queued || b.requests - a.requests);
    else if (tenantSort === "tokens") c.sort((a, b) => b.tokens - a.tokens);
    else c.sort((a, b) => b.requests - a.requests);
    return c.slice(0, 8);
  }, [tenantRows, tenantSort]);

  const sortedKeys = useMemo(() => {
    const c = [...keyRows];
    if (keySort === "tokens") c.sort((a, b) => b.tokens - a.tokens);
    else c.sort((a, b) => b.requests - a.requests);
    return c.slice(0, 8);
  }, [keyRows, keySort]);

  return (
    <Card className="h-full rounded-md">
      <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <CardTitle>Metrics explorer</CardTitle>
          <CardDescription>
            {view === "models" ? "Throughput and latency per model" : view === "tenants" ? "Live tenant pressure" : "Busiest API keys"}
          </CardDescription>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <div className="inline-flex rounded-sm border border-border bg-background/40 p-0.5">
            <MetricToggle active={view === "models"} label="Models" onClick={() => setView("models")} />
            <MetricToggle active={view === "tenants"} label="Tenants" onClick={() => setView("tenants")} />
            <MetricToggle active={view === "keys"} label="Keys" onClick={() => setView("keys")} />
          </div>
        </div>
      </CardHeader>
      <CardContent>
        {view === "models" ? (
          <div className="space-y-3">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <ModelSortSelect sort={sort} onChange={setSort} />
              <div className="inline-flex rounded-sm border border-border bg-background/40 p-0.5">
                {(Object.keys(WINDOW_MS) as ModelWindow[]).map((w) => (
                  <MetricToggle key={w} active={activeWindow === w} label={w} onClick={() => onWindowChange(w)} />
                ))}
              </div>
            </div>
            {sortedModels.length === 0 ? (
              <EmptyState className="h-72">No model traffic in this window</EmptyState>
            ) : (
              <div className="space-y-2">
                {sortedModels.map((row) => (
                  <ModelExplorerRow
                    key={row.model}
                    row={row}
                    expanded={expanded === row.model}
                    onToggle={() => setExpanded((c) => (c === row.model ? null : row.model))}
                  />
                ))}
              </div>
            )}
          </div>
        ) : view === "tenants" ? (
          <div className="space-y-2">
            <div className="inline-flex rounded-sm border border-border bg-background/40 p-0.5">
              <MetricToggle active={tenantSort === "requests"} label="Requests" onClick={() => setTenantSort("requests")} />
              <MetricToggle active={tenantSort === "queued"} label="Queued" onClick={() => setTenantSort("queued")} />
              <MetricToggle active={tenantSort === "tokens"} label="Tokens" onClick={() => setTenantSort("tokens")} />
            </div>
            <SimpleEntityList
              rows={sortedTenants.map((r) => ({
                key: r.id,
                label: r.name,
                detail: `${r.group} / ${formatCompact(r.tokens)} tokens`,
                value: formatNumber(r.requests),
                sub: `${formatNumber(r.inFlight)} running / ${formatNumber(r.queued)} queued`,
                color: r.color,
                weight: r.requests + r.inFlight + r.queued,
              }))}
              empty="No tenant traffic in the last hour"
            />
          </div>
        ) : (
          <div className="space-y-2">
            <div className="inline-flex rounded-sm border border-border bg-background/40 p-0.5">
              <MetricToggle active={keySort === "requests"} label="Requests" onClick={() => setKeySort("requests")} />
              <MetricToggle active={keySort === "tokens"} label="Tokens" onClick={() => setKeySort("tokens")} />
            </div>
            <SimpleEntityList
              rows={sortedKeys.map((r) => ({
                key: r.keyId,
                label: `key ${r.keyLabel}`,
                detail: `${r.tenant} / ${r.group}`,
                value: formatNumber(r.requests),
                sub: `${formatCompact(r.tokens)} tokens`,
                color: r.color,
                weight: r.requests,
              }))}
              empty="No key traffic in the last hour"
            />
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function ModelSortSelect({ sort, onChange }: { sort: ModelSort; onChange: (s: ModelSort) => void }) {
  return (
    <select
      value={sort}
      onChange={(e) => onChange(e.target.value as ModelSort)}
      aria-label="Sort models"
      className="h-8 rounded-sm border border-border bg-background/40 px-2 text-xs text-foreground"
    >
      <option value="requests">Requests</option>
      <option value="genTps">Gen tok/s</option>
      <option value="aggTps">Aggregate tok/s</option>
      <option value="ttft">TTFB p50</option>
      <option value="e2e">E2E p50</option>
      <option value="tokens">Tokens</option>
      <option value="users">Users</option>
    </select>
  );
}

function sortModelRows(rows: ModelDisplayRow[], sort: ModelSort): ModelDisplayRow[] {
  const copy = [...rows];
  switch (sort) {
    case "genTps": return copy.sort((a, b) => b.genTps - a.genTps);
    case "aggTps": return copy.sort((a, b) => b.aggTps - a.aggTps);
    case "ttft": return copy.sort((a, b) => a.p50TtftMs - b.p50TtftMs);
    case "e2e": return copy.sort((a, b) => a.p50TotalMs - b.p50TotalMs);
    case "tokens": return copy.sort((a, b) => b.tokens - a.tokens);
    case "users": return copy.sort((a, b) => b.users - a.users);
    case "requests":
    default: return copy.sort((a, b) => b.requests - a.requests);
  }
}

function ModelExplorerRow({
  row,
  expanded,
  onToggle,
}: {
  row: ModelDisplayRow;
  expanded: boolean;
  onToggle: () => void;
}) {
  const statusTone: MetricTone = row.status === "unhealthy" ? "hot" : row.status === "unknown" || row.status === "disabled" ? "warn" : "neutral";
  const slotLabel = row.slots == null ? `${formatNumber(row.inFlight)} / ∞` : `${formatNumber(row.inFlight)} / ${formatNumber(row.slots)}`;

  return (
    <div className="rounded-sm border border-border bg-background/30">
      <button
        type="button"
        aria-expanded={expanded}
        onClick={onToggle}
        className="w-full px-3 py-2 text-left transition-colors hover:bg-muted/20"
      >
        <div className="flex items-center gap-3">
          <span className="text-xs text-muted-foreground">{expanded ? "▾" : "▸"}</span>
          <span className="min-w-0 flex-1 truncate text-xs font-medium">{row.model}</span>
          <StripMetric label="req" value={formatNumber(row.requests)} />
          <StripMetric label="gen tok/s" value={formatDecimal(row.genTps)} tone={row.genTps > 0 && row.genTps < 10 ? "warn" : "neutral"} />
          <StripMetric label="TTFB" value={`${formatNumber(Math.round(row.p50TtftMs))}ms`} tone={row.p50TtftMs > 1000 ? "warn" : "neutral"} />
          <StripMetric label="queued" value={formatNumber(row.queued)} tone={row.queued > 0 ? "warn" : "neutral"} />
        </div>
      </button>
      {expanded && (
        <div className="border-t border-dashed border-border px-3 py-3">
          <div className="grid grid-cols-2 gap-2 text-xs md:grid-cols-4">
            <DetailStat label="Aggregate tok/s" value={`${formatCompact(row.aggTps)} tok/s`} />
            <DetailStat label="E2E p50" value={`${formatNumber(Math.round(row.p50TotalMs))} ms`} />
            <DetailStat label="TTFB avg" value={`${formatNumber(Math.round(row.avgTtftMs))} ms`} />
            <DetailStat label="E2E avg" value={`${formatNumber(Math.round(row.avgTotalMs))} ms`} />
            <DetailStat label="In / Slots" value={slotLabel} />
            <DetailStat label="Queued" value={formatNumber(row.queued)} tone={row.queued > 0 ? "warn" : "neutral"} />
            <DetailStat label="In tokens" value={formatCompact(row.inputTokens)} />
            <DetailStat label="Out tokens" value={formatCompact(row.outputTokens)} />
            <DetailStat label="Avg prompt" value={`${formatCompact(row.avgPromptTokens)} tok`} />
            <DetailStat label="Avg gen" value={`${formatCompact(row.avgGenTokens)} tok`} />
            <DetailStat label="Users" value={formatNumber(row.users)} />
            <DetailStat label="Status" value={row.status} tone={statusTone} />
          </div>
          <div className="mt-3">
            <Link href={`/models?model=${encodeURIComponent(row.model)}`} className="text-xs text-muted-foreground underline underline-offset-2 hover:text-foreground">
              Open in Models
            </Link>
          </div>
        </div>
      )}
    </div>
  );
}

function StripMetric({ label, value, tone = "neutral" }: { label: string; value: string; tone?: MetricTone }) {
  const toneClass = tone === "hot" ? "text-[hsl(350_55%_64%)]" : tone === "warn" ? "text-[hsl(38_65%_62%)]" : "text-foreground";
  return (
    <div className="shrink-0 text-right">
      <p className={cn("text-xs font-medium tabular-nums", toneClass)}>{value}</p>
      <p className="text-[10px] uppercase tracking-wider text-muted-foreground">{label}</p>
    </div>
  );
}

function SimpleEntityList({
  rows,
  empty,
}: {
  rows: { key: string; label: string; detail: string; value: string; sub: string; color: string; weight: number }[];
  empty: string;
}) {
  const maxWeight = rows.reduce((m, r) => Math.max(m, r.weight), 0);
  if (rows.length === 0) return <EmptyState className="h-72">{empty}</EmptyState>;
  return (
    <div className="space-y-2">
      {rows.map((row) => (
        <div key={row.key} className="rounded-sm border border-border bg-background/30 px-3 py-2">
          <div className="flex items-center justify-between gap-3">
            <div className="min-w-0">
              <p className="truncate text-xs font-medium">{row.label}</p>
              <p className="mt-0.5 truncate text-[11px] text-muted-foreground">{row.detail}</p>
            </div>
            <div className="shrink-0 text-right">
              <p className="text-xs font-medium tabular-nums">{row.value}</p>
              <p className="text-[11px] tabular-nums text-muted-foreground">{row.sub}</p>
            </div>
          </div>
          <div className="mt-2 h-1.5 overflow-hidden rounded-sm bg-muted/35">
            <div className="h-full rounded-sm" style={{ width: `${clamp(maxWeight > 0 ? (row.weight / maxWeight) * 100 : 0, 0, 100)}%`, background: row.color }} />
          </div>
        </div>
      ))}
    </div>
  );
}

function TenantPanel({ rows }: { rows: TenantDisplayRow[] }) {
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const selected = rows.find((row) => row.id === selectedId) ?? null;
  const maxRequests = rows.reduce((max, row) => Math.max(max, row.requests), 0);

  return (
    <Card className="h-full rounded-md">
      <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <CardTitle>Tenant load</CardTitle>
          <CardDescription>Last hour, with live fairshare state</CardDescription>
        </div>
        <Button type="button" variant="outline" size="sm" asChild>
          <Link href="/fairshare">
            <Gauge className="h-3.5 w-3.5" />
            Fairshare
          </Link>
        </Button>
      </CardHeader>
      <CardContent>
        {rows.length === 0 ? (
          <EmptyState className="h-72">No tenant traffic in the last hour</EmptyState>
        ) : (
          <div className="space-y-4">
            <div className="max-h-80 space-y-2 overflow-auto pr-1">
              {rows.slice(0, TOP_TENANTS).map((row) => (
                <UsageButton
                  key={row.id}
                  label={row.name}
                  detail={`${row.group} / ${formatCompact(row.tokens)} tokens`}
                  value={formatNumber(row.requests)}
                  sub={`${formatNumber(row.inFlight)} running / ${formatNumber(row.queued)} queued`}
                  color={row.color}
                  pct={maxRequests > 0 ? (row.requests / maxRequests) * 100 : 0}
                  active={selectedId === row.id}
                  onClick={() => setSelectedId((current) => (current === row.id ? null : row.id))}
                />
              ))}
            </div>
            {selected && <TenantDetail row={selected} />}
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function TenantDetail({ row }: { row: TenantDisplayRow }) {
  return (
    <div className="rounded-sm border border-border bg-background/35 p-3">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="truncate text-sm font-medium">{row.name}</p>
          <p className="mt-0.5 truncate text-xs text-muted-foreground">
            {row.group} / weight {formatNumber(row.weight)}
          </p>
        </div>
        <Link href={`/tenants?tenant=${encodeURIComponent(row.id)}`} className="shrink-0 text-xs text-muted-foreground underline underline-offset-2 hover:text-foreground">
          Tenants
        </Link>
      </div>
      <div className="mt-3 grid grid-cols-2 gap-2 text-xs md:grid-cols-4">
        <DetailStat label="Requests" value={formatNumber(row.requests)} />
        <DetailStat label="Tokens" value={formatCompact(row.tokens)} />
        <DetailStat label="Expected" value={formatDecimal(row.expectedSlots)} />
        <DetailStat label="Gap" value={formatDelta(row.inFlight - row.expectedSlots)} tone={row.queued > 0 && row.inFlight < row.expectedSlots ? "hot" : "neutral"} />
      </div>
    </div>
  );
}

function ModelPanel({ rows }: { rows: ModelDisplayRow[] }) {
  const [selectedModel, setSelectedModel] = useState<string | null>(null);
  const selected = rows.find((row) => row.model === selectedModel) ?? null;
  const maxRequests = rows.reduce((max, row) => Math.max(max, row.requests), 0);

  return (
    <Card className="h-full rounded-md">
      <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <CardTitle>Model load</CardTitle>
          <CardDescription>Last hour, joined to route health</CardDescription>
        </div>
        <Button type="button" variant="outline" size="sm" asChild>
          <Link href="/models">
            <Boxes className="h-3.5 w-3.5" />
            Models
          </Link>
        </Button>
      </CardHeader>
      <CardContent>
        {rows.length === 0 ? (
          <EmptyState className="h-72">No model traffic in the last hour</EmptyState>
        ) : (
          <div className="space-y-4">
            <div className="max-h-80 space-y-2 overflow-auto pr-1">
              {rows.slice(0, TOP_MODELS).map((row, i) => (
                <UsageButton
                  key={row.model}
                  label={row.model}
                  detail={`${formatDecimal(row.genTps)} tok/s/stream / in ${formatCompact(row.inputTokens)} / out ${formatCompact(row.outputTokens)}`}
                  value={formatNumber(row.requests)}
                  sub={row.status}
                  color={PALETTE[i % PALETTE.length]}
                  pct={maxRequests > 0 ? (row.requests / maxRequests) * 100 : 0}
                  active={selectedModel === row.model}
                  onClick={() => setSelectedModel((current) => (current === row.model ? null : row.model))}
                />
              ))}
            </div>
            {selected && <ModelDetail row={selected} />}
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function ModelDetail({ row }: { row: ModelDisplayRow }) {
  const route = row.route;
  const avgTokens = row.requests > 0 ? row.tokens / row.requests : 0;

  return (
    <div className="rounded-sm border border-border bg-background/35 p-3">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="truncate text-sm font-medium">{row.model}</p>
          <p className="mt-0.5 truncate text-xs text-muted-foreground">
            {formatNumber(row.requests)} requests / avg {formatCompact(avgTokens)} tokens
          </p>
        </div>
        <Link href={`/models?model=${encodeURIComponent(row.model)}`} className="shrink-0 text-xs text-muted-foreground underline underline-offset-2 hover:text-foreground">
          Models
        </Link>
      </div>
      <div className="mt-3 grid grid-cols-2 gap-2 text-xs md:grid-cols-4">
        <DetailStat label="Status" value={row.status} tone={row.status === "unhealthy" ? "hot" : row.status === "unknown" ? "warn" : "neutral"} />
        <DetailStat label="Tokens" value={formatCompact(row.tokens)} />
        <DetailStat label="Stream tok/s" value={`${formatDecimal(row.genTps)} tok/s`} />
        <DetailStat label="Aggregate tok/s" value={`${formatCompact(row.aggTps)} tok/s`} />
        <DetailStat label="TTFB" value={`${formatNumber(Math.round(row.avgTtftMs))} ms`} />
        <DetailStat label="E2E" value={`${formatNumber(Math.round(row.avgTotalMs))} ms`} />
        <DetailStat label="TTFB p50" value={`${formatNumber(Math.round(row.p50TtftMs))} ms`} />
        <DetailStat label="E2E p50" value={`${formatNumber(Math.round(row.p50TotalMs))} ms`} />
        <DetailStat label="Avg prompt" value={`${formatCompact(row.avgPromptTokens)} tok`} />
        <DetailStat label="Avg gen" value={`${formatCompact(row.avgGenTokens)} tok`} />
        <DetailStat label="Users" value={formatNumber(row.users)} />
        <DetailStat label="Weight" value={route ? formatNumber(route.admission_weight) : "--"} />
        <DetailStat label="Slots" value={route?.max_in_flight == null ? "unlimited" : formatNumber(route.max_in_flight)} />
        {route && <DetailStat className="md:col-span-4" label="Upstream" value={route.upstream_model} />}
      </div>
    </div>
  );
}

function KeyPanel({ rows }: { rows: KeyDisplayRow[] }) {
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const selected = rows.find((row) => row.keyId === selectedKey) ?? null;
  const maxRequests = rows.reduce((max, row) => Math.max(max, row.requests), 0);

  return (
    <Card className="flex h-full min-h-[32rem] flex-col rounded-md">
      <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <CardTitle>Busiest keys</CardTitle>
          <CardDescription>Last hour / top {TOP_KEYS}</CardDescription>
        </div>
        <Button type="button" variant="outline" size="sm" asChild>
          <Link href="/keys">
            <KeyRound className="h-3.5 w-3.5" />
            Keys
          </Link>
        </Button>
      </CardHeader>
      <CardContent className="flex min-h-0 flex-1 flex-col">
        {rows.length === 0 ? (
          <EmptyState className="min-h-0 flex-1">No key traffic in the last hour</EmptyState>
        ) : (
          <div className="flex min-h-0 flex-1 flex-col space-y-4">
            <div className="min-h-0 flex-1 space-y-2 overflow-auto pr-1">
              {rows.map((row) => (
                <UsageButton
                  key={row.keyId}
                  label={`key ${row.keyLabel}`}
                  detail={`${row.tenant} / ${row.group}`}
                  value={formatNumber(row.requests)}
                  sub={`${formatCompact(row.tokens)} tokens`}
                  color={row.color}
                  pct={maxRequests > 0 ? (row.requests / maxRequests) * 100 : 0}
                  active={selectedKey === row.keyId}
                  onClick={() => setSelectedKey((current) => (current === row.keyId ? null : row.keyId))}
                />
              ))}
            </div>
            {selected && <KeyDetail row={selected} />}
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function KeyDetail({ row }: { row: KeyDisplayRow }) {
  const avgTokens = row.requests > 0 ? row.tokens / row.requests : 0;

  return (
    <div className="rounded-sm border border-border bg-background/35 p-3">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="truncate font-mono text-sm">key {row.keyLabel}</p>
          <p className="mt-0.5 truncate text-xs text-muted-foreground">
            {row.tenant} / {row.group} / avg {formatCompact(avgTokens)} tokens
          </p>
        </div>
        <Link href={`/keys?key=${encodeURIComponent(row.keyLabel)}`} className="shrink-0 text-xs text-muted-foreground underline underline-offset-2 hover:text-foreground">
          Keys
        </Link>
      </div>
      <div className="mt-3 grid grid-cols-2 gap-2 text-xs">
        <DetailStat label="Requests" value={formatNumber(row.requests)} />
        <DetailStat label="Tokens" value={formatCompact(row.tokens)} />
      </div>
    </div>
  );
}

function RequestFeedPanel() {
  const query = useQuery({
    queryKey: ["overview-request-feed"],
    queryFn: () =>
      getJson<UsageLogEntry[]>(
        `/api/live/usage/logs?since_ms=${Date.now() - HOUR_MS}&limit=${LIVE_REQUEST_ROWS}`,
      ),
    refetchInterval: REQUEST_LOG_POLL_MS,
  });
  const rows = (query.data ?? []).slice(0, LIVE_REQUEST_ROWS);
  const errorCount = rows.filter((row) => row.status_code >= 400).length;

  return (
    <Card className="h-full rounded-md">
      <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <CardTitle>Live requests</CardTitle>
          <CardDescription>Last hour / newest {LIVE_REQUEST_ROWS} / refresh {REQUEST_LOG_POLL_MS / 1000}s</CardDescription>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Badge className={errorCount > 0 ? "border-red-500/35 bg-red-500/10 text-red-300" : ""}>
            {errorCount > 0 ? `${formatNumber(errorCount)} errors` : "healthy"}
          </Badge>
          <Button type="button" variant="outline" size="sm" asChild>
            <Link href="/logs">
              <Radio className="h-3.5 w-3.5" />
              Logs
            </Link>
          </Button>
        </div>
      </CardHeader>
      <CardContent className="p-0">
        {rows.length === 0 ? (
          <EmptyState className="h-72">{query.isLoading ? "Loading requests..." : "No recent requests"}</EmptyState>
        ) : (
          <div className="overflow-hidden">
            <table className="w-full min-w-[720px] text-xs">
              <thead className="bg-card">
                <tr className="border-b border-border text-left text-[10px] uppercase tracking-wider text-muted-foreground">
                  <th className="px-4 py-2 font-medium">Time</th>
                  <th className="px-3 py-2 font-medium">Status</th>
                  <th className="px-3 py-2 font-medium">Type</th>
                  <th className="px-3 py-2 font-medium">Model</th>
                  <th className="px-3 py-2 font-medium">Team</th>
                  <th className="px-3 py-2 text-right font-medium">Tokens</th>
                  <th className="px-4 py-2 text-right font-medium">Duration</th>
                </tr>
              </thead>
              <tbody>
                {rows.map((row) => (
                  <RequestFeedRow key={`${row.request_id}-${row.ts_ms}`} row={row} />
                ))}
              </tbody>
            </table>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function RequestFeedRow({ row }: { row: UsageLogEntry }) {
  const error = row.status_code >= 400;
  const totalMs = Number(row.total_ms);
  const tokenCount = Number(row.total_tokens);

  return (
    <tr className="border-b border-border/60 transition-colors hover:bg-muted/20">
      <td className="px-4 py-2 tabular-nums text-muted-foreground">
        {new Date(row.ts_ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" })}
      </td>
      <td className="px-3 py-2">
        <span
          className={cn(
            "inline-flex h-5 items-center rounded-sm border border-border px-1.5 text-[10px] font-medium tabular-nums",
            error ? "border-red-500/35 bg-red-500/10 text-red-300" : "bg-background/35 text-muted-foreground",
          )}
        >
          {error ? row.status_code : "ok"}
        </span>
      </td>
      <td className="px-3 py-2 font-mono text-[11px]">{row.request_type || "request"}</td>
      <td className="max-w-44 truncate px-3 py-2 text-muted-foreground">{row.model || "unknown"}</td>
      <td className="max-w-36 truncate px-3 py-2 text-muted-foreground">
        {row.tenant_name || row.tenant_id.slice(0, 8)}
        <span className="text-muted-foreground/50"> / </span>
        {row.key_name || row.key_prefix || row.key_id.slice(0, 8)}
      </td>
      <td className="px-3 py-2 text-right tabular-nums">{formatCompact(tokenCount)}</td>
      <td className="px-4 py-2 text-right tabular-nums text-muted-foreground">
        {Number.isFinite(totalMs) && totalMs > 0 ? `${formatNumber(Math.round(totalMs))} ms` : formatCurrency(Number(row.cost_usd) || 0)}
      </td>
    </tr>
  );
}

function UsageButton({
  label,
  detail,
  value,
  sub,
  color,
  pct,
  active,
  onClick,
}: {
  label: string;
  detail: string;
  value: string;
  sub: string;
  color: string;
  pct: number;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      aria-pressed={active}
      onClick={onClick}
      className={cn(
        "w-full rounded-sm border border-border bg-background/30 px-3 py-2 text-left transition-colors hover:bg-muted/20",
        active && "border-muted-foreground/40 bg-muted/15",
      )}
    >
      <div className="flex items-center justify-between gap-3">
        <div className="min-w-0">
          <p className="truncate text-xs font-medium">{label}</p>
          <p className="mt-0.5 truncate text-[11px] text-muted-foreground">{detail}</p>
        </div>
        <div className="text-right">
          <p className="text-xs font-medium tabular-nums">{value}</p>
          <p className="text-[11px] tabular-nums text-muted-foreground">{sub}</p>
        </div>
      </div>
      <div className="mt-2 h-1.5 overflow-hidden rounded-sm bg-muted/35">
        <div className="h-full rounded-sm" style={{ width: `${clamp(pct, 0, 100)}%`, background: color }} />
      </div>
    </button>
  );
}

function MetricToggle({ active, onClick, label }: { active: boolean; onClick: () => void; label: string }) {
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

function DetailStat({
  label,
  value,
  tone = "neutral",
  className,
}: {
  label: string;
  value: string;
  tone?: MetricTone;
  className?: string;
}) {
  const toneClass =
    tone === "hot" ? "text-[hsl(350_55%_64%)]" : tone === "warn" ? "text-[hsl(38_65%_62%)]" : "text-foreground";
  return (
    <div className={cn("min-w-0 rounded-sm border border-border/70 bg-card/35 px-2 py-1.5", className)}>
      <p className="text-[10px] uppercase tracking-wider text-muted-foreground">{label}</p>
      <p className={cn("mt-0.5 truncate font-mono text-[11px] tabular-nums", toneClass)}>{value}</p>
    </div>
  );
}

function EmptyState({ children, className = "h-64" }: { children: React.ReactNode; className?: string }) {
  return <p className={cn("flex items-center justify-center text-center text-sm text-muted-foreground", className)}>{children}</p>;
}

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

interface CacheSummary {
  hits: number;
  misses: number;
  hitRate: number;
  hitRateLabel: string;
  tokensSaved: number;
}

function summarizeCache(stats?: CacheStats): CacheSummary {
  const hits = stats?.hits ?? 0;
  const misses = stats?.misses ?? 0;
  const total = hits + misses;
  const hitRate = total > 0 ? (hits / total) * 100 : 0;
  return {
    hits,
    misses,
    hitRate,
    hitRateLabel: total > 0 ? formatPct(hitRate) : "idle",
    tokensSaved: stats?.tokens_saved ?? 0,
  };
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

interface TenantDisplayRow {
  id: string;
  name: string;
  group: string;
  requests: number;
  tokens: number;
  weight: number;
  inFlight: number;
  queued: number;
  expectedSlots: number;
  color: string;
}

function buildTenantRows(
  tenantSeries: TenantUsageTimePoint[],
  fallbackUsage: UsageAgg[],
  tenants: Tenant[],
  view?: FairshareLiveView,
): TenantDisplayRow[] {
  const totals = new Map<string, { requests: number; tokens: number }>();
  for (const point of tenantSeries) {
    const current = totals.get(point.tenant_id) ?? { requests: 0, tokens: 0 };
    current.requests += Number(point.requests);
    current.tokens += Number(point.total_tokens);
    totals.set(point.tenant_id, current);
  }
  if (totals.size === 0) {
    for (const usage of fallbackUsage) {
      totals.set(usage.tenant_id, {
        requests: Number(usage.requests),
        tokens: Number(usage.total_tokens),
      });
    }
  }

  const fairshare = new Map((view?.tenants ?? []).map((tenant) => [tenant.tenant_id, tenant]));

  return tenants
    .map((tenant, index) => {
      const total = totals.get(tenant.id) ?? { requests: 0, tokens: 0 };
      const live = fairshare.get(tenant.id);
      return {
        id: tenant.id,
        name: tenant.name,
        group: tenant.fairshare_group || "default",
        requests: total.requests,
        tokens: total.tokens,
        weight: live?.weight ?? tenant.weight,
        inFlight: live?.in_flight ?? 0,
        queued: live?.queued ?? 0,
        expectedSlots: live?.expected_slots ?? 0,
        color: colorForGroup(tenant.fairshare_group || "default", index),
      };
    })
    .filter((row) => row.requests > 0 || row.inFlight > 0 || row.queued > 0)
    .sort((a, b) => b.requests - a.requests || b.queued - a.queued || b.tokens - a.tokens);
}

interface ModelDisplayRow {
  model: string;
  requests: number;
  inputTokens: number;
  outputTokens: number;
  tokens: number;
  genTps: number;
  aggTps: number;
  avgTtftMs: number;
  avgTotalMs: number;
  p50TtftMs: number;
  p50TotalMs: number;
  avgPromptTokens: number;
  avgGenTokens: number;
  users: number;
  inFlight: number;
  queued: number;
  slots: number | null; // null = unlimited (no max_in_flight cap)
  status: string;
  route?: ModelRoute;
}

function buildModelRows(
  usage: UsageModelAgg[],
  routes: ModelRoute[],
  health: ModelHealthSummary[],
  fairshare?: FairshareLiveView,
): ModelDisplayRow[] {
  const routeByName = new Map(routes.map((route) => [route.model_name, route]));
  const healthById = new Map(health.map((row) => [row.model_id, row]));
  const inFlightByModel = fairshare?.model_in_flight ?? {};
  const queuedByModel = fairshare?.model_queued ?? {};

  return usage
    .filter((row) => !isBenchmarkModelName(row.model))
    .map((row) => {
      const route = routeByName.get(row.model);
      const summary = route ? healthById.get(route.id) : undefined;
      return {
        model: row.model,
        requests: Number(row.requests),
        inputTokens: Number(row.input_tokens),
        outputTokens: Number(row.output_tokens),
        tokens: Number(row.total_tokens),
        genTps: Number(row.gen_tokens_per_sec),
        aggTps: Number(row.agg_tokens_per_sec),
        avgTtftMs: Number(row.avg_ttft_ms),
        avgTotalMs: Number(row.avg_total_ms),
        p50TtftMs: Number(row.p50_ttft_ms),
        p50TotalMs: Number(row.p50_total_ms),
        avgPromptTokens: Number(row.avg_prompt_tokens),
        avgGenTokens: Number(row.avg_gen_tokens),
        users: Number(row.users),
        inFlight: inFlightByModel[row.model] ?? 0,
        queued: queuedByModel[row.model] ?? 0,
        slots: route?.max_in_flight ?? null,
        status: route && !route.enabled ? "disabled" : summary ? healthStatus(summary) : route ? "unknown" : "unrouted",
        route,
      };
    })
    .filter((row) => row.requests > 0 || row.inFlight > 0 || row.queued > 0)
    .sort((a, b) => b.requests - a.requests || b.tokens - a.tokens);
}

interface KeyDisplayRow {
  keyId: string;
  keyLabel: string;
  tenant: string;
  group: string;
  requests: number;
  tokens: number;
  color: string;
}

function buildKeyRows(
  usage: UsageKeyAgg[],
  tenantNames: Record<string, string>,
  tenantGroups: Record<string, string>,
): KeyDisplayRow[] {
  return usage
    .map((row, index) => {
      const group = tenantGroups[row.tenant_id] || "default";
      return {
        keyId: row.key_id,
        keyLabel: row.key_id.slice(0, 8),
        tenant: tenantNames[row.tenant_id] ?? row.tenant_id.slice(0, 8),
        group,
        requests: Number(row.requests),
        tokens: Number(row.total_tokens),
        color: colorForGroup(group, index),
      };
    })
    .filter((row) => row.requests > 0)
    .sort((a, b) => b.requests - a.requests || b.tokens - a.tokens)
    .slice(0, TOP_KEYS);
}

function colorForGroup(name: string, index = 0) {
  return GROUP_PALETTE[name] ?? PALETTE[index % PALETTE.length];
}

function isWaitingBelowShare(tenant: FairshareLiveView["tenants"][number]) {
  return tenant.queued > 0 && tenant.in_flight < Math.floor(tenant.expected_slots);
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

async function getJson<T>(url: string): Promise<T> {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`fetch failed: ${url}`);
  return (await res.json()) as T;
}

function formatPct(n: number): string {
  if (!Number.isFinite(n)) return "0%";
  if (Math.abs(n) < 10 && n !== 0) return `${n.toFixed(1)}%`;
  return `${Math.round(n)}%`;
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

function formatDelta(delta: number): string {
  const rounded = Math.abs(delta) < 0.05 ? 0 : delta;
  if (rounded === 0) return "0";
  const sign = rounded > 0 ? "+" : "-";
  const mag = Math.abs(rounded);
  return `${sign}${mag >= 10 || Number.isInteger(mag) ? formatNumber(Math.round(mag)) : mag.toFixed(1)}`;
}

function clamp(n: number, min: number, max: number) {
  return Math.min(max, Math.max(min, n));
}
