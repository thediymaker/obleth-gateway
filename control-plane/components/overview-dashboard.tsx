"use client";

import Link from "next/link";
import { useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, Boxes, Gauge, KeyRound, RefreshCw, ScrollText, Users } from "lucide-react";
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
  AuditEntry,
  CacheStats,
  FairshareLiveView,
  LiveStats,
  ModelHealthSummary,
  ModelRoute,
  Tenant,
  TenantUsageTimePoint,
  UsageAgg,
  UsageKeyAgg,
  UsageModelAgg,
  UsageTimePoint,
} from "@/lib/obleth";
import { cn, formatCurrency, formatNumber } from "@/lib/utils";

const SUMMARY_POLL_MS = 30_000;
const FAST_POLL_MS = 2_000;
const STATS_POLL_MS = 5_000;
const USAGE_POLL_MS = 20_000;
const SLOW_POLL_MS = 60_000;

const DAY_MS = 86_400_000;
const HOUR_MS = 3_600_000;
const LIVE_BUCKET_MS = 60_000;

const TOP_TENANTS = 8;
const TOP_MODELS = 8;
const TOP_KEYS = 10;

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
const HOT_COLOR = "hsl(350 55% 64%)";
const OK_COLOR = "hsl(160 14% 58%)";

type TrafficRange = "live" | "day";
type MetricTone = "ok" | "warn" | "hot" | "neutral";

export function OverviewDashboard({
  tenants,
  models,
  initialSummary,
  initialVolumeSeries,
  initialTenantUsage,
  initialTenantSeries,
  initialModelUsage,
  initialKeyUsage,
  initialAudit,
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
  initialAudit: AuditEntry[];
  initialCacheStats?: CacheStats;
  initialHealth: ModelHealthSummary[];
  initialFairshare?: FairshareLiveView;
  initialStats?: LiveStats;
}) {
  const queryClient = useQueryClient();
  const [trafficRange, setTrafficRange] = useState<TrafficRange>("live");

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
    queryKey: ["usage-models-top"],
    queryFn: () => getJson<UsageModelAgg[]>(`/api/live/usage/models?since_ms=${Date.now() - HOUR_MS}`),
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
    () => buildModelRows(modelUsageQuery.data ?? [], visibleModels, activeHealth),
    [modelUsageQuery.data, visibleModels, activeHealth],
  );
  const keyRows = useMemo(
    () => buildKeyRows(keyUsageQuery.data ?? [], tenantNames, tenantGroups),
    [keyUsageQuery.data, tenantNames, tenantGroups],
  );

  const capacity = summarizeCapacity(fairshare, stats);
  const healthSummary = summarizeHealth(activeHealth, visibleModels);
  const cacheSummary = summarizeCache(activeCache);

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
          fairshare={fairshare}
          health={healthSummary}
          cache={cacheSummary}
          modelCount={visibleModels.length}
        />
      </div>

      <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
        <TenantPanel rows={tenantRows} />
        <ModelPanel rows={modelRows} />
      </div>

      <div className="grid gap-4 xl:grid-cols-[minmax(22rem,0.82fr)_minmax(0,1.18fr)]">
        <KeyPanel rows={keyRows} />
        <RecentChangesPanel entries={initialAudit} />
      </div>
    </div>
  );
}

function OverviewConsoleHeader({
  fairshare,
  isFetching,
  onRefresh,
}: {
  fairshare?: FairshareLiveView;
  isFetching: boolean;
  onRefresh: () => void;
}) {
  return (
    <div className="rounded-md border border-border bg-card px-4 py-3">
      <div className="flex flex-col gap-3 md:flex-row md:items-center md:justify-between">
        <div className="flex flex-wrap items-center gap-2">
          <Badge className="capitalize">{fairshare?.algorithm ?? "loading"} admission</Badge>
          <Badge className="gap-1.5">
            <span className="h-1.5 w-1.5 rounded-full bg-[hsl(160_14%_58%)]" />
            live
          </Badge>
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
      label: "Capacity",
      value: capacity.max > 0 ? `${formatNumber(capacity.inFlight)} / ${formatNumber(capacity.max)}` : "--",
      sub: capacity.max > 0 ? `${formatPct(capacity.utilization)} used` : "waiting",
      tone: capacity.tone,
    },
    {
      label: "Queue",
      value: formatNumber(capacity.queued),
      sub: capacity.waitingTenants > 0 ? `${formatNumber(capacity.waitingTenants)} tenants waiting` : "no backlog",
      tone: capacity.queued > 0 ? "warn" : "ok",
    },
    {
      label: "Requests 24h",
      value: formatCompact(summary.requests),
      sub: `${formatNumber(summary.activeTenants)} active tenants`,
      tone: "neutral",
    },
    {
      label: "Tokens 24h",
      value: formatCompact(summary.tokens),
      sub: avgTokens > 0 ? `avg ${formatCompact(avgTokens)} / request` : "input + output",
      tone: "neutral",
    },
    {
      label: "Spend 24h",
      value: summary.hasPricing ? formatCurrency(summary.cost) : "--",
      sub: summary.hasPricing ? "priced routes" : "pricing not set",
      tone: "neutral",
    },
    {
      label: "Model health",
      value: `${formatNumber(health.healthy)} / ${formatNumber(health.enabled)}`,
      sub: health.unhealthy > 0 ? `${formatNumber(health.unhealthy)} unhealthy` : `${formatNumber(health.unknown)} unknown`,
      tone: health.unhealthy > 0 ? "hot" : health.unknown > 0 ? "warn" : "ok",
    },
    {
      label: "Tenants",
      value: formatNumber(summary.tenantCount),
      sub: `${formatNumber(summary.activeTenants)} active / ${formatNumber(activeGroups)} groups`,
      tone: "neutral",
    },
    {
      label: "API keys",
      value: formatCompact(summary.keyCount),
      sub: `cache ${cache.hitRateLabel}`,
      tone: "neutral",
    },
  ] satisfies MetricTile[];

  return (
    <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
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
  fairshare,
  health,
  cache,
  modelCount,
}: {
  summary: OverviewSummary;
  capacity: CapacitySummary;
  fairshare?: FairshareLiveView;
  health: HealthSummary;
  cache: CacheSummary;
  modelCount: number;
}) {
  const busiestGroup = fairshare?.groups
    .slice()
    .sort((a, b) => b.in_flight + b.queued - (a.in_flight + a.queued))[0];
  const slotLabel = capacity.max > 0 ? `${formatPct(capacity.utilization)} occupied` : "waiting";

  return (
    <Card className="h-full rounded-md">
      <CardHeader>
        <CardTitle>Gateway now</CardTitle>
        <CardDescription>Scheduler, fleet, and cache state</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div>
          <div className="mb-2 flex items-center justify-between gap-3 text-xs">
            <span className="text-muted-foreground">Slots</span>
            <span className="tabular-nums">{slotLabel}</span>
          </div>
          <SlotBar pct={capacity.utilization} tone={capacity.tone} />
          <div className="mt-2 grid grid-cols-3 gap-2 text-xs">
            <DetailStat label="In flight" value={formatNumber(capacity.inFlight)} />
            <DetailStat label="Queued" value={formatNumber(capacity.queued)} tone={capacity.queued > 0 ? "warn" : "neutral"} />
            <DetailStat label="Headroom" value={formatNumber(capacity.headroom)} />
          </div>
        </div>

        <div className="grid grid-cols-2 gap-2 text-xs">
          <DetailStat label="Tenants" value={`${formatNumber(summary.activeTenants)} / ${formatNumber(summary.tenantCount)}`} />
          <DetailStat label="Models" value={`${formatNumber(health.enabled)} / ${formatNumber(modelCount)}`} />
          <DetailStat label="Keys" value={formatNumber(summary.keyCount)} />
          <DetailStat label="Cache" value={cache.hitRateLabel} />
        </div>

        <div className="rounded-sm border border-border bg-background/35 p-3">
          <div className="flex items-start justify-between gap-3">
            <div className="min-w-0">
              <p className="text-xs font-medium">Fairshare pressure</p>
              <p className="mt-0.5 truncate text-[11px] text-muted-foreground">
                {busiestGroup
                  ? `${busiestGroup.name} / ${formatNumber(busiestGroup.in_flight)} running / ${formatNumber(busiestGroup.queued)} queued`
                  : "No active group pressure"}
              </p>
            </div>
            <Badge className={capacity.queued > 0 ? "border-amber-500/35 bg-amber-500/10 text-amber-300" : ""}>
              {capacity.waitingTenants > 0 ? `${formatNumber(capacity.waitingTenants)} waiting` : "clear"}
            </Badge>
          </div>
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
            <Link href="/tenants">
              <Users className="h-3.5 w-3.5" />
              Tenants
            </Link>
          </Button>
          <Button type="button" variant="outline" size="sm" asChild>
            <Link href="/keys">
              <KeyRound className="h-3.5 w-3.5" />
              Keys
            </Link>
          </Button>
        </div>
      </CardContent>
    </Card>
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
            Scheduler
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
            Routes
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
        <DetailStat label="TTFT" value={`${formatNumber(Math.round(row.avgTtftMs))} ms`} />
        <DetailStat label="E2E" value={`${formatNumber(Math.round(row.avgTotalMs))} ms`} />
        <DetailStat label="TTFT p50" value={`${formatNumber(Math.round(row.p50TtftMs))} ms`} />
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

function RecentChangesPanel({ entries }: { entries: AuditEntry[] }) {
  return (
    <Card className="h-full rounded-md">
      <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <CardTitle>Recent changes</CardTitle>
          <CardDescription>Latest configuration events</CardDescription>
        </div>
        <Button type="button" variant="outline" size="sm" asChild>
          <Link href="/audit">
            <ScrollText className="h-3.5 w-3.5" />
            Audit
          </Link>
        </Button>
      </CardHeader>
      <CardContent className="p-0">
        {entries.length === 0 ? (
          <EmptyState className="h-72">No audit events yet</EmptyState>
        ) : (
          <ul className="divide-y divide-border/60">
            {entries.map((entry) => (
              <li key={entry.id} className="flex items-baseline justify-between gap-4 px-6 py-3 text-sm">
                <div className="min-w-0">
                  <p className="truncate font-mono text-xs">{entry.action}</p>
                  <p className="mt-0.5 truncate text-muted-foreground">
                    {entry.entity_type} / {entry.actor}
                  </p>
                </div>
                <time className="shrink-0 text-xs tabular-nums text-muted-foreground">
                  {new Date(entry.ts).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" })}
                </time>
              </li>
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
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

function SlotBar({ pct, tone }: { pct: number; tone: MetricTone }) {
  const color = tone === "hot" ? HOT_COLOR : tone === "warn" ? REQUEST_COLOR : OK_COLOR;
  return (
    <div className="h-2 overflow-hidden rounded-sm bg-muted/35">
      <div className="h-full rounded-sm transition-[width]" style={{ width: `${clamp(pct, 0, 100)}%`, background: color }} />
    </div>
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
  status: string;
  route?: ModelRoute;
}

function buildModelRows(usage: UsageModelAgg[], routes: ModelRoute[], health: ModelHealthSummary[]): ModelDisplayRow[] {
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
        status: route && !route.enabled ? "disabled" : summary ? healthStatus(summary) : route ? "unknown" : "unrouted",
        route,
      };
    })
    .filter((row) => row.requests > 0)
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
