"use client";

import Link from "next/link";
import { useEffect, useMemo, useRef, useState, useTransition } from "react";
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
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { colorForGroup, OTHERS_COLOR, PALETTE } from "@/lib/chart-palette";
import { isWaitingBelowShare } from "@/lib/fairshare";
import { clamp, formatCompact, formatDecimal, formatPct, formatScore } from "@/lib/format";
import type {
  FairshareHistoryPoint,
  FairshareHistoryView,
  FairshareLiveView,
  GroupFairshareView,
  KeyFairshareView,
  ModelPoolView,
  ModelRoute,
  TenantFairshareView,
} from "@/lib/obleth";
import { cn, formatNumber } from "@/lib/utils";

export type {
  FairshareLiveView,
  GroupFairshareView,
  KeyFairshareView,
  ModelPoolView,
  TenantFairshareView,
} from "@/lib/obleth";

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

const QUEUED_COLOR = "hsl(38 75% 60%)";

type ThroughputMetric = "requests" | "tokens";
type TenantSort = "pressure" | "queued" | "deficit" | "served" | "score" | "weight" | "share";
type TenantScope = "all" | "active" | "waiting" | "starved";

// ---------------------------------------------------------------------------
// Root
// ---------------------------------------------------------------------------

/** Present one pool with the same shape as the all-models view so every panel
 *  can render either without knowing which it got. */
export function scopedView(view: FairshareLiveView | undefined, model: string): FairshareLiveView | undefined {
  if (!view || model === "all") return view;
  const pool = view.pools?.find((p) => p.model === model);
  if (!pool) return view;
  return {
    ...view,
    max_in_flight: pool.cap,
    global_in_flight: pool.in_flight,
    global_queued: pool.queued,
    global_borrowed: pool.borrowed,
    groups: pool.groups,
    tenants: pool.tenants,
    keys: pool.keys,
    model_in_flight: { [pool.model]: pool.in_flight },
    model_queued: { [pool.model]: pool.queued },
  };
}

export function FairshareDashboard({
  tenantNames,
}: {
  tenantNames: Record<string, string>;
}) {
  const queryClient = useQueryClient();
  const { data: rawView, isFetching, isError, dataUpdatedAt } = useFairshareLive();
  const { data: tenantSeries } = useThroughputSeries();
  const { data: modelRoutes } = useModelRoutes();
  const [throughputMetric, setThroughputMetric] = useState<ThroughputMetric>("requests");
  const [section, setSection] = useState("overview");
  const [scope, setScope] = useState("all");
  const { history, groupKeys, oldestTs, retentionMs } = useFairshareHistory(scope);
  const [tenantFilter, setTenantFilter] = useState({ group: "all", scope: "active" as TenantScope });
  const inspectTenants = (group: string, scope: TenantScope) => {
    setTenantFilter({ group, scope });
    setSection("tenants");
  };

  const view = useMemo(() => scopedView(rawView, scope), [rawView, scope]);
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
        isError={isError}
        dataUpdatedAt={dataUpdatedAt}
        onRefresh={refresh}
      />

      {rawView?.pools && rawView.pools.length > 0 && (
        <label className="flex items-center gap-2 text-xs text-muted-foreground">
          Model
          <select
            aria-label="Model scope"
            value={scope}
            onChange={(e) => setScope(e.target.value)}
            className="rounded-md border border-input bg-background px-2 py-1 text-sm text-foreground"
          >
            <option value="all">All models</option>
            {[...rawView.pools].sort((a, b) => a.model.localeCompare(b.model)).map((p) => (
              <option key={p.model} value={p.model}>{p.model} · {p.in_flight}/{p.cap}</option>
            ))}
          </select>
        </label>
      )}
      <PressureStrip view={view} summary={summary} />
      <Tabs value={section} onValueChange={setSection}>
        <TabsList aria-label="Fairshare views" className="h-auto max-w-full flex-wrap justify-start">
          <TabsTrigger value="overview"><Activity className="h-3.5 w-3.5" />Overview</TabsTrigger>
          <TabsTrigger value="allocation"><Network className="h-3.5 w-3.5" />Allocation</TabsTrigger>
          <TabsTrigger value="tenants"><Users className="h-3.5 w-3.5" />Tenants</TabsTrigger>
        </TabsList>
        <TabsContent value="overview" className="space-y-4">
          <WaitingTenants view={view} onShowAll={() => inspectTenants("all", "waiting")} />
          <details className="rounded-md border border-border bg-card">
            <summary className="cursor-pointer px-4 py-3 text-sm font-medium">Activity history</summary>
            <div className="grid gap-4 p-4 pt-0 xl:grid-cols-2">
              <CapacityTimeline history={history} groups={groupKeys} oldestTs={oldestTs} retentionMs={retentionMs} view={view} />
              <ThroughputPanel data={throughput.data} series={throughput.series} metric={throughputMetric} onMetricChange={setThroughputMetric} />
            </div>
          </details>
        </TabsContent>
        <TabsContent value="allocation" className="space-y-4">
          <GroupAllocation view={view} onInspect={(group) => inspectTenants(group, "all")} />
          <ModelSlotPressure view={view} routes={modelRoutes ?? []} />
        </TabsContent>
        <TabsContent value="tenants">
          <TenantOperations key={`${tenantFilter.group}:${tenantFilter.scope}`} view={view} initialGroup={tenantFilter.group} initialScope={tenantFilter.scope} />
        </TabsContent>
      </Tabs>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Data hooks
// ---------------------------------------------------------------------------

interface GroupHistoryPoint {
  time: string;
  queued: number;
  [key: string]: number | string;
}

interface GroupKey {
  name: string;
  key: string;
  color: string;
}

/** Cache of formatted clock times keyed by `ts_ms`, so re-running
 *  `buildHistoryChart` over an appended history only formats the new points
 *  instead of the whole retained window every poll. Bounded so a long-lived
 *  tab does not grow this without limit. */
const TIME_LABEL_CACHE_MAX = 4096;
const timeLabelCache = new Map<number, string>();

function formatPointTime(tsMs: number): string {
  let label = timeLabelCache.get(tsMs);
  if (label === undefined) {
    if (timeLabelCache.size >= TIME_LABEL_CACHE_MAX) timeLabelCache.clear();
    label = new Date(tsMs).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
    timeLabelCache.set(tsMs, label);
  }
  return label;
}

const MAX_CHART_ROWS = 600;

/** Downsample `rows` to at most `max` entries, always keeping the last row so
 *  the chart's right edge stays current. Returns `rows` itself, unchanged,
 *  when it is already within budget. */
export function thinHistory<T>(rows: T[], max: number): T[] {
  if (rows.length <= max) return rows;
  const step = Math.ceil(rows.length / max);
  const thinned: T[] = [];
  for (let i = 0; i < rows.length; i += step) thinned.push(rows[i]);
  const last = rows[rows.length - 1];
  if (thinned[thinned.length - 1] !== last) {
    // Swap in the last row rather than appending when we are already at
    // budget, so the result never exceeds `max`.
    if (thinned.length >= max) thinned[thinned.length - 1] = last;
    else thinned.push(last);
  }
  return thinned;
}

function useFairshareLive() {
  return useQuery({
    queryKey: ["fairshare-live"],
    queryFn: async () => {
      const res = await fetch("/api/live/fairshare");
      if (!res.ok) throw new Error("fairshare unavailable");
      return (await res.json()) as FairshareLiveView;
    },
    refetchInterval: FAIRSHARE_POLL_MS,
  });
}

function historyQs(params: { model?: string; since_ms?: number }) {
  const q = new URLSearchParams();
  if (params.model) q.set("model", params.model);
  if (params.since_ms !== undefined) q.set("since_ms", String(params.since_ms));
  const s = q.toString();
  return s ? `?${s}` : "";
}

export async function fetchHistory(model: string | undefined, sinceMs?: number) {
  const res = await fetch(`/api/live/fairshare/history${historyQs({ model, since_ms: sinceMs })}`);
  if (!res.ok) throw new Error("fairshare history unavailable");
  return (await res.json()) as FairshareHistoryView;
}

/** Merge tail points onto `prev`: only strictly newer timestamps are added,
 *  and points older than `retentionMs` before the newest are dropped. Returns
 *  `prev` itself when nothing changes. */
export function appendHistoryTail(
  prev: FairshareHistoryPoint[],
  fresh: FairshareHistoryPoint[],
  retentionMs: number,
): FairshareHistoryPoint[] {
  const newest = prev.length ? prev[prev.length - 1].ts_ms : -Infinity;
  const added = fresh.filter((p) => p.ts_ms > newest);
  if (added.length === 0) return prev;
  const merged = [...prev, ...added];
  const cutoff = retentionMs > 0 ? merged[merged.length - 1].ts_ms - retentionMs : -Infinity;
  return merged.filter((p) => p.ts_ms >= cutoff);
}

/** Project server points into the stacked chart's rows and the sorted group
 *  series. Every row carries every group so the stack never has holes. */
export function buildHistoryChart(points: FairshareHistoryPoint[]): {
  history: GroupHistoryPoint[];
  groupKeys: GroupKey[];
} {
  const names = new Set<string>();
  for (const p of points) for (const g of Object.keys(p.groups)) names.add(g);
  const groupKeys: GroupKey[] = [...names]
    .sort((a, b) => a.localeCompare(b))
    .map((name, i) => ({ name, key: groupDataKey(name), color: colorForGroup(name, i) }));
  const history = points.map((p) => {
    const row: GroupHistoryPoint = {
      time: formatPointTime(p.ts_ms),
      queued: p.queued,
    };
    for (const g of groupKeys) row[g.key] = p.groups[g.name] ?? 0;
    return row;
  });
  return { history, groupKeys };
}

/** Retained scheduler history for one scope ("all" or a model name): the
 *  full window on mount and scope change, then the tail every poll. */
function useFairshareHistory(scope: string) {
  const model = scope === "all" ? undefined : scope;
  const [points, setPoints] = useState<FairshareHistoryPoint[]>([]);
  const [oldestTs, setOldestTs] = useState<number | null>(null);
  const [retentionMs, setRetentionMs] = useState<number | null>(null);
  const newestRef = useRef<number | null>(null);

  useEffect(() => {
    setPoints([]);
    setOldestTs(null);
    newestRef.current = null;
  }, [scope]);

  const full = useQuery({
    queryKey: ["fairshare-history", scope],
    queryFn: () => fetchHistory(model),
  });
  useEffect(() => {
    if (!full.data || !Array.isArray(full.data.points)) return;
    setPoints(full.data.points);
    setOldestTs(full.data.oldest_ts_ms);
    setRetentionMs(full.data.retention_ms);
    newestRef.current = full.data.points.length ? full.data.points[full.data.points.length - 1].ts_ms : null;
  }, [full.data]);

  const tail = useQuery({
    queryKey: ["fairshare-history-tail", scope],
    queryFn: () => fetchHistory(model, newestRef.current === null ? undefined : newestRef.current + 1),
    refetchInterval: FAIRSHARE_POLL_MS,
    // Not `full.isSuccess`: a failed initial fetch must not leave the tail
    // disabled forever. With `newestRef.current` still null in that case, the
    // tail requests the full window itself and rehydrates through
    // `appendHistoryTail`.
    enabled: !full.isPending,
  });
  useEffect(() => {
    if (!tail.data || !Array.isArray(tail.data.points)) return;
    setOldestTs(tail.data.oldest_ts_ms);
    setRetentionMs(tail.data.retention_ms);
    setPoints((prev) => {
      const next = appendHistoryTail(prev, tail.data.points, tail.data.retention_ms);
      if (next !== prev && next.length) newestRef.current = next[next.length - 1].ts_ms;
      return next;
    });
  }, [tail.data]);

  // The header's "History since" must not claim a time earlier than the
  // first plotted point, nor hide a point the ring already reports as
  // retained: take whichever of the two is more recent, or fall back to
  // whichever one exists.
  const displayOldestTs = useMemo(() => {
    const firstPointTs = points.length ? points[0].ts_ms : null;
    if (oldestTs !== null && firstPointTs !== null) return Math.min(oldestTs, firstPointTs);
    return oldestTs ?? firstPointTs;
  }, [oldestTs, points]);

  const chart = useMemo(() => buildHistoryChart(points), [points]);
  return { ...chart, oldestTs: displayOldestTs, retentionMs };
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
  starvedTenants: number;
  waitingTenants: number;
}

function summarizeFairshare(view?: FairshareLiveView): FairshareSummary {
  if (!view) {
    return {
      utilization: 0,
      starvedTenants: 0,
      waitingTenants: 0,
    };
  }

  const waiting = view.tenants.filter((t) => t.queued > 0);
  const starved = waiting.filter(isWaitingBelowShare);


  return {
    utilization: view.max_in_flight > 0 ? (view.global_in_flight / view.max_in_flight) * 100 : 0,
    starvedTenants: starved.length,
    waitingTenants: waiting.length,
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
  isError,
  dataUpdatedAt,
  onRefresh,
}: {
  view?: FairshareLiveView;
  summary: FairshareSummary;
  isFetching: boolean;
  isError: boolean;
  dataUpdatedAt: number;
  onRefresh: () => void;
}) {
  const tone = pressureStatus(summary.utilization, view?.global_queued ?? 0);
  const label =
    isError ? "Scheduler unavailable" : !view ? "Loading scheduler" : tone === "hot"
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
          <p className="mt-0.5 text-sm text-muted-foreground">Capacity, contention, and who is waiting.</p>
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
            {view && slotModeBadge(view) && (
              <Badge
                data-testid="slot-mode"
                className={cn(view.mode === "fallback" && "border-amber-500/35 bg-amber-500/10 text-amber-300")}
              >
                {slotModeBadge(view)}
              </Badge>
            )}
            <Badge className="gap-1.5">
              <span className="h-1.5 w-1.5 rounded-full bg-[hsl(160_14%_58%)]" />
              poll {FAIRSHARE_POLL_MS / 1000}s
            </Badge>
          </div>
          <p className="mt-2 text-xs text-muted-foreground">{detail}</p>
          {view && limitsNote(view) && (
            <p className={cn("mt-1 text-xs", view.mode === "fallback" ? "text-amber-300" : "text-muted-foreground")}>
              {limitsNote(view)}
            </p>
          )}
          {isError && <p role="alert" className="mt-2 text-xs text-amber-400">{view ? `Showing the last snapshot from ${new Date(dataUpdatedAt).toLocaleTimeString()}. Refresh failed; retrying automatically.` : "Could not load scheduler state. Retrying automatically."}</p>}
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



function PressureStrip({ view, summary }: { view?: FairshareLiveView; summary: FairshareSummary }) {
  const items = [
    {
      label: "Active / capacity",
      value: view ? `${formatNumber(view.global_in_flight)} / ${formatNumber(view.max_in_flight)}` : "--",
      sub: view ? `${formatPct(summary.utilization)} of global slots occupied` : "Waiting for scheduler state",
      tone: "neutral",
    },
    {
      label: "Queued requests",
      value: view ? formatNumber(view.global_queued) : "--",
      sub:
        view && view.global_queued > 0
          ? `${formatNumber(summary.waitingTenants)} tenants waiting`
          : view ? "No backlog" : "Waiting for scheduler state",
      tone: view && view.global_queued > 0 ? "warn" : "ok",
    },
    {
      label: "Waiting below share",
      value: view ? formatNumber(summary.starvedTenants) : "--",
      sub: view ? `${formatDecimal(view.tenants.filter(isWaitingBelowShare).reduce((sum, t) => sum + t.expected_slots - t.in_flight, 0))} slots below expected` : "Waiting for scheduler state",
      tone: summary.starvedTenants > 0 ? "hot" : "ok",
    },
  ] satisfies MetricTile[];

  return (
    <div className="grid grid-cols-1 gap-3 md:grid-cols-3">
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
  oldestTs,
  retentionMs,
  view,
}: {
  history: GroupHistoryPoint[];
  groups: GroupKey[];
  oldestTs: number | null;
  retentionMs: number | null;
  view?: FairshareLiveView;
}) {
  const rows = thinHistory(history, MAX_CHART_ROWS);
  return (
    <Card className="h-full rounded-md">
      <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <CardTitle>Live occupancy</CardTitle>
          <CardDescription>Group activity over the retained window; queued work uses the right axis</CardDescription>
          <p className="mt-1 text-xs text-muted-foreground">
            {retentionMs === 0 && oldestTs === null
              ? "History disabled (OBLETH_FAIRSHARE_HISTORY_SECS=0)"
              : oldestTs === null
                ? "No samples yet"
                : `History since ${new Date(oldestTs).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" })}`}
          </p>
        </div>
        {view && (
          <div className="flex flex-wrap justify-start gap-2 text-xs sm:justify-end">
            <Badge>{formatNumber(view.global_in_flight)} in flight</Badge>
            <Badge>{formatNumber(view.global_queued)} queued</Badge>
          </div>
        )}
      </CardHeader>
      <CardContent>
        {rows.length === 0 ? (
          <EmptyState className="h-72">Waiting for live scheduler samples</EmptyState>
        ) : (
          <ChartShell heightClass="h-72">
            <ResponsiveContainer width="100%" height="100%">
              <ComposedChart data={rows} margin={{ top: 8, right: 12, left: 4, bottom: 4 }}>
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

function WaitingTenants({ view, onShowAll }: { view?: FairshareLiveView; onShowAll: () => void }) {
  const [selectedId, setSelectedId] = useState<string>();
  const waiting = [...(view?.tenants ?? [])].filter((t) => t.queued > 0).sort((a, b) =>
    Number(isWaitingBelowShare(b)) - Number(isWaitingBelowShare(a)) || fairnessGap(a) - fairnessGap(b) || b.queued - a.queued || a.tenant_id.localeCompare(b.tenant_id),
  );
  const shown = waiting.slice(0, 8);
  const selected = view?.tenants.find((t) => t.tenant_id === selectedId) ?? shown[0];
  const scale = Math.max(1, ...shown.map((t) => Math.max(t.in_flight, t.expected_slots)));
  return (
    // Both cards stretch to the row's height, so the list and the inspector
    // stay level whether either one is empty or populated.
    <div className="grid items-stretch gap-4 xl:grid-cols-[minmax(0,1.7fr)_minmax(18rem,1fr)]">
      <Card className="flex min-w-0 flex-col rounded-md">
        <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
          <div><CardTitle>Waiting tenants</CardTitle><CardDescription>Tenants waiting below their expected share appear first.</CardDescription></div>
          <Button variant="outline" size="sm" onClick={onShowAll}>View all waiting ({formatNumber(waiting.length)})</Button>
        </CardHeader>
        <CardContent className="flex flex-1 flex-col">
          {!view ? <EmptyState className="flex-1">Waiting for scheduler state</EmptyState> : shown.length === 0 ? <EmptyState className="flex-1">No tenants are waiting for admission</EmptyState> : (
            <ul className="divide-y divide-border">
              {shown.map((tenant) => (
                <li key={tenant.tenant_id}>
                  <button type="button" aria-pressed={selected?.tenant_id === tenant.tenant_id} onClick={() => setSelectedId(tenant.tenant_id)} className={cn("grid w-full gap-3 rounded-sm px-2 py-4 text-left transition-colors hover:bg-muted/30 sm:grid-cols-[minmax(0,1fr)_minmax(0,1.2fr)_4rem] sm:items-center", selected?.tenant_id === tenant.tenant_id && "bg-muted/30")}>
                    <span className="min-w-0"><span className="block break-words text-sm font-medium">{tenant.name}</span><span className="block break-words text-xs text-muted-foreground">{tenant.fairshare_group}</span>{isWaitingBelowShare(tenant) && <span className="text-xs text-amber-400">Waiting below share</span>}</span>
                    <SlotComparison tenant={tenant} scale={scale} />
                    <span className="text-right text-sm tabular-nums">{formatNumber(tenant.queued)}<span className="block text-xs text-muted-foreground">queued</span></span>
                  </button>
                </li>
              ))}
            </ul>
          )}
          <p className="mt-auto pt-4 text-xs text-muted-foreground">Bar: active slots. Marker: expected share, not a hard limit. {shown.length > 0 && `Shared scale: 0–${formatDecimal(scale)} slots.`}</p>
          {waiting.length > shown.length && <p className="mt-2 text-xs text-muted-foreground">Showing {shown.length} of {formatNumber(waiting.length)} waiting tenants.</p>}
        </CardContent>
      </Card>
      <TenantInspector key={selected?.tenant_id ?? "empty"} tenant={selected} view={view} />
    </div>
  );
}

function SlotComparison({ tenant, scale }: { tenant: TenantFairshareView; scale: number }) {
  return (
    <span className="block min-w-0" aria-label={`${tenant.in_flight} active, ${formatDecimal(tenant.expected_slots)} expected slots`}>
      <span className="mb-2 flex flex-wrap justify-between gap-x-2 text-xs tabular-nums text-muted-foreground"><span>{formatNumber(tenant.in_flight)} active</span><span>{formatDecimal(tenant.expected_slots)} expected</span></span>
      <span className="relative block h-2 rounded-sm bg-muted/40">
        <span className="block h-full rounded-sm bg-foreground/50" style={{ width: `${clamp(tenant.in_flight / scale * 100, 0, 100)}%` }} />
        <span aria-hidden className="absolute -top-1 h-4 w-px -translate-x-full bg-foreground" style={{ left: `${clamp(tenant.expected_slots / scale * 100, 0, 100)}%` }} />
      </span>
    </span>
  );
}

function TenantInspector({ tenant, view }: { tenant?: TenantFairshareView; view?: FairshareLiveView }) {
  return (
    <Card className="flex min-w-0 flex-col rounded-md" aria-label="Tenant details">
      <CardHeader><CardTitle className="break-words">{tenant?.name ?? "Tenant details"}</CardTitle><CardDescription className="break-all">{tenant?.tenant_id ?? "Select a tenant to inspect its allocation."}</CardDescription></CardHeader>
      {/* The same minimum height whether or not a tenant is selected, so the
          card does not resize when the selection changes; a long key list
          still grows it. */}
      <CardContent className="flex min-h-[27rem] flex-1 flex-col">
        {tenant ? <>
          <p className={cn("mb-4 rounded-sm bg-muted/30 px-3 py-2 text-xs", isWaitingBelowShare(tenant) && "text-amber-400")}>
            {isWaitingBelowShare(tenant) ? `Waiting · ${formatDecimal(-fairnessGap(tenant))} slots below expected` : tenant.queued > 0 ? "Waiting for admission" : "No queued requests"}
          </p>
          <dl className="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)] gap-x-3 gap-y-3 text-xs">
            <dt className="text-muted-foreground">Group</dt><dd className="break-words text-right">{tenant.fairshare_group}</dd>
            <dt className="text-muted-foreground">Active / expected</dt><dd className="text-right tabular-nums">{formatNumber(tenant.in_flight)} / {formatDecimal(tenant.expected_slots)}</dd>
            <dt className="text-muted-foreground">Queued</dt><dd className="text-right tabular-nums">{formatNumber(tenant.queued)}</dd>
            <dt className="text-muted-foreground">Weight share</dt><dd className="text-right tabular-nums">{formatPct(tenant.weight_share * 100)}</dd>
            <dt className="text-muted-foreground">Scheduler debt</dt><dd className="text-right tabular-nums">{formatScore(tenantDebt(view, tenant))}</dd>
            <dt className="text-muted-foreground">Served tokens</dt><dd className="text-right tabular-nums">{formatCompact(tenant.served_tokens)}</dd>
            <dt className="text-muted-foreground">Tenant weight</dt><dd><WeightCell id={tenant.tenant_id} weight={tenant.weight} /></dd>
          </dl>
          {view?.keys && (
            <div className="mt-4">
              <p className="mb-2 text-xs font-medium">Keys</p>
              {view.keys.filter((k) => k.tenant_id === tenant.tenant_id).length === 0 ? (
                <p className="text-xs text-muted-foreground">No active keys</p>
              ) : (
                <table className="w-full text-xs tabular-nums" aria-label="Tenant keys">
                  <thead><tr className="text-muted-foreground"><th className="text-left font-normal">Key</th><th className="text-right font-normal">Active</th><th className="text-right font-normal">Queued</th><th className="text-right font-normal">Served</th><th className="text-right font-normal">Weight</th><th className="text-right font-normal">Cap</th></tr></thead>
                  <tbody>
                    {[...view.keys.filter((k) => k.tenant_id === tenant.tenant_id)]
                      .sort((a, b) => a.share_score - b.share_score)
                      .map((k) => (
                        <tr key={k.key_id}>
                          <td className="truncate text-left">{k.name}</td>
                          <td className="text-right">{formatNumber(k.in_flight)}</td>
                          <td className="text-right">{formatNumber(k.queued)}</td>
                          <td className="text-right">{formatCompact(k.served_tokens)}</td>
                          <td className="text-right">{k.weight}</td>
                          <td className="text-right">{k.max_in_flight ?? "–"}</td>
                        </tr>
                      ))}
                  </tbody>
                </table>
              )}
            </div>
          )}
          <p className="mt-4 text-xs text-muted-foreground">Weights change relative priority; they do not reserve slots. Admission also depends on scheduler debt and model capacity.</p>
        </> : <EmptyState className="flex-1">No tenant selected</EmptyState>}
      </CardContent>
    </Card>
  );
}


// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

function GroupAllocation({ view, onInspect }: { view?: FairshareLiveView; onInspect: (group: string) => void }) {
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
          <CardDescription>Groups can borrow idle capacity above their apportioned cap. Inspect a group to see its tenants.</CardDescription>
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
              <GroupAllocationRow key={g.name} group={g} index={i} onInspect={() => onInspect(g.name)} />
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
  onInspect,
}: {
  group: GroupFairshareView;
  index: number;
  onInspect: () => void;
}) {
  const color = colorForGroup(group.name, index);
  const cap = Math.max(group.slot_cap, group.expected_slots, group.in_flight, 1);
  const activePct = clamp((group.in_flight / cap) * 100, 0, 100);

  return (
    <div className="rounded-md border border-border bg-background/25 px-4 py-3">
      <div className="grid gap-3 lg:grid-cols-[minmax(11rem,0.8fr)_minmax(0,1.6fr)_minmax(18rem,1fr)] lg:items-center">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <span className="h-2.5 w-2.5 shrink-0 rounded-full" style={{ background: color }} />
            <button type="button" onClick={onInspect} className="break-words text-left text-sm font-semibold underline decoration-border underline-offset-4 hover:decoration-foreground" aria-label={`Inspect ${group.name} tenants`}>{group.name}</button>
          </div>
          <p className="mt-1 text-xs tabular-nums text-muted-foreground">
            w{formatNumber(group.weight)} / {formatPct(group.weight_share * 100)} share
          </p>
        </div>

        <div className="min-w-0">
          <div className="mb-1 flex items-center justify-between gap-2 text-[11px] tabular-nums text-muted-foreground">
            <span>{formatNumber(group.in_flight)} active</span>
            <span>{formatDecimal(group.expected_slots)} expected / apportioned cap {formatNumber(group.slot_cap)}</span>
          </div>
          <div className="relative h-3 rounded-sm bg-muted/35">
            <div className="h-full rounded-sm" style={{ width: `${activePct}%`, background: color }} />
            <span
              className="absolute top-[-2px] h-[calc(100%+4px)] w-px -translate-x-full bg-foreground/70"
              style={{ left: `${clamp((group.expected_slots / cap) * 100, 0, 100)}%` }}
            />
          </div>
          <p className="mt-2 text-[11px] text-muted-foreground">Bar: active slots · marker: expected · scale: 0–{formatDecimal(cap)}</p>
        </div>

        <div className="grid grid-cols-4 gap-2 text-right">
          <GroupNumber label="Queued" value={formatNumber(group.queued)} tone={group.queued > 0 ? "warn" : "neutral"} />
          <GroupNumber label="Served" value={formatCompact(group.served_tokens)} />
          <GroupNumber label="Debt" value={formatScore(group.share_score)} />
          <GroupNumber label="Borrowed" value={formatNumber(Math.max(0, group.in_flight - group.slot_cap))} />
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

/** A gateway's share of a cluster-wide limit in split mode, as the gateway computes it. */
export function replicaShare(configured: number, replicas: number): number {
  return Math.max(Math.ceil(configured / Math.max(replicas, 1)), 1);
}

/** What the answering gateway divides the configured limits by in its current mode. */
export function limitDivisor(view: FairshareLiveView): number {
  const n = Math.max(view.replicas ?? 1, 1);
  switch (view.mode) {
    case "split":
      return n;
    case "fallback":
      return view.replica_aware === false ? 1 : n;
    case "shared":
    case "local":
      return 1;
    default:
      return view.replica_aware ? n : 1;
  }
}

/** Short label for the header badge; empty for a single gateway. */
export function slotModeBadge(view: FairshareLiveView): string {
  const n = Math.max(view.replicas ?? 1, 1);
  switch (view.mode) {
    case "shared":
      return `Shared slots · ${formatNumber(n)} gateways`;
    case "fallback":
      return view.replica_aware === false ? "Fallback · per gateway" : "Fallback · split";
    case "split":
      return `Split across ${formatNumber(n)} gateways`;
    default:
      return "";
  }
}

/** One line saying whose numbers the page shows and how the limits hold across gateways. */
export function limitsNote(view: FairshareLiveView): string {
  const n = Math.max(view.replicas ?? 1, 1);
  const here = `this gateway ${formatNumber(view.global_in_flight)}`;
  switch (view.mode) {
    case "shared": {
      const configured = view.configured_max_in_flight ?? view.max_in_flight;
      const cluster = view.cluster_in_flight == null ? "—" : formatNumber(view.cluster_in_flight);
      return `In flight ${cluster} of ${formatNumber(configured)} (cluster-wide, ${formatNumber(n)} gateways) · ${here}. Any gateway can use a model's whole pool; pool sizes, tenant and key caps and the ceiling hold across all of them. Queues and fairness below are this gateway's.`;
    }
    case "fallback":
      return view.replica_aware === false
        ? `Fallback: shared slots are unavailable, so each of ${formatNumber(n)} gateways enforces the full configured limits until Redis answers again. Counts are this gateway's own.`
        : `Fallback: shared slots are unavailable, so each of ${formatNumber(n)} gateways enforces ceil(configured / ${formatNumber(n)}) of every limit until Redis answers again. Counts are this gateway's own.`;
    case "split":
      return `Shared slots are off: each of ${formatNumber(n)} live gateways enforces ceil(configured / ${formatNumber(n)}) of every limit. Counts are this gateway's own.`;
    case "local":
      return n > 1
        ? `Shared slots and replica-aware sizing are off: each of ${formatNumber(n)} gateways enforces the full configured limits. Counts are this gateway's own.`
        : "1 live gateway: pools, caps and the ceiling are enforced at their configured size.";
    default:
      return "";
  }
}

function ModelSlotPressure({ view, routes }: { view?: FairshareLiveView; routes: ModelRoute[] }) {
  const divisor = view ? limitDivisor(view) : 1;
  const shared = view?.mode === "shared";
  const rows = useMemo(() => {
    const inFlight = view?.model_in_flight ?? {};
    const queued = view?.model_queued ?? {};
    const cluster = new Map(
      (view?.pools ?? []).filter((p) => p.cluster_in_flight != null).map((p) => [p.model, p.cluster_in_flight as number]),
    );
    const capByName = new Map(routes.map((r) => [r.model_name, r.max_in_flight ?? null]));
    const names = new Set<string>([...Object.keys(inFlight), ...Object.keys(queued)]);
    if (shared) for (const [name, n] of cluster) if (n > 0) names.add(name);
    return [...names]
      .map((name) => {
        const configured = capByName.get(name) ?? null;
        const here = inFlight[name] ?? 0;
        return {
          name,
          // Shared slots: measure the cluster-wide count against the whole
          // pool. Otherwise counts are this gateway's, against what it enforces.
          inFlight: shared ? (cluster.get(name) ?? here) : here,
          here,
          queued: queued[name] ?? 0,
          cap: configured === null ? null : replicaShare(configured, divisor),
          configured,
          capKnown: capByName.has(name),
        };
      })
      .filter((r) => r.inFlight > 0 || r.queued > 0)
      .sort((a, b) => b.queued - a.queued || b.inFlight - a.inFlight);
  }, [view, routes, divisor, shared]);

  return (
    <Card className="rounded-md">
      <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <CardTitle>Model slot pressure</CardTitle>
          <CardDescription>Live in-flight and queued requests against each model's slot cap.</CardDescription>
        </div>
        <div className="flex flex-wrap gap-2 text-[11px] text-muted-foreground">
          <span className="inline-flex items-center gap-1.5"><i className="h-2 w-2 rounded-sm bg-[hsl(205_55%_52%)]" />in-flight</span>
          <span>Queued requests are shown separately from occupied slots.</span>
        </div>
      </CardHeader>
      <CardContent>
        {rows.length === 0 ? (
          <EmptyState className="h-32">No models with active or queued requests</EmptyState>
        ) : (
          <div className="space-y-2.5">
            {rows.map((r) => {
              const cap = Math.max(r.cap ?? r.inFlight, 1);
              const inflightPct = clamp((r.inFlight / cap) * 100, 0, 100);
              return (
                <div key={r.name} className="grid gap-2 text-xs sm:grid-cols-[minmax(0,1fr)_minmax(0,2fr)_minmax(0,1fr)] sm:items-center">
                  <span className="break-words font-medium">{r.name}</span>
                  <div className="h-3 overflow-hidden rounded-sm bg-muted/35">
                    {r.capKnown && r.cap !== null && <div className="h-full bg-[hsl(205_55%_52%)] transition-all duration-500" style={{ width: `${inflightPct}%` }} />}
                  </div>
                  <span className="tabular-nums text-muted-foreground sm:text-right">
                    {formatNumber(r.inFlight)} active{shared ? " cluster-wide" : ""} / {!r.capKnown ? "cap unavailable" : r.cap == null ? "no model cap" : `${formatNumber(r.cap)} cap`}
                    {shared && <span className="block">{formatNumber(r.here)} on this gateway</span>}
                    {divisor > 1 && r.configured !== null && <span className="block">of {formatNumber(r.configured)} configured</span>}
                    <span className={cn("block", r.queued > 0 && "text-amber-400")}>{formatNumber(r.queued)} queued</span>
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



function TenantOperations({ view, initialGroup, initialScope }: { view?: FairshareLiveView; initialGroup: string; initialScope: TenantScope }) {
  const [query, setQuery] = useState("");
  const [groupFilter, setGroupFilter] = useState(initialGroup);
  const [scope, setScope] = useState<TenantScope>(initialScope);
  const [selectedId, setSelectedId] = useState<string>();
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
  const selected = rows.find((t) => t.tenant_id === selectedId) ?? shown[0];
  const starved = useMemo(() => rows.filter(isWaitingBelowShare).length, [rows]);
  const totalActive = view?.tenants.filter(isTenantActive).length ?? 0;
  const totalWaiting = view?.tenants.filter((t) => t.queued > 0).length ?? 0;
  const totalStarved = view?.tenants.filter(isWaitingBelowShare).length ?? 0;

  return (
    <div className="space-y-4">
    <Card className="min-w-0 rounded-md">
      <CardHeader className="gap-3 lg:flex-row lg:items-start lg:justify-between">
        <div>
          <CardTitle>Tenant workbench</CardTitle>
          <CardDescription>
            {view ? `${formatNumber(rows.length)} tenant${rows.length === 1 ? "" : "s"}` : "Loading tenants"}
            {rows.length > TENANT_PAGE ? ` / showing ${formatNumber(TENANT_PAGE)}` : ""}
            {starved > 0 ? ` / ${formatNumber(starved)} waiting below share` : ""}
          </CardDescription>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <div className="inline-flex flex-wrap rounded-sm border border-border bg-background/35 p-0.5">
            <ScopeToggle active={scope === "active"} label={`Active ${formatNumber(totalActive)}`} onClick={() => setScope("active")} />
            <ScopeToggle active={scope === "waiting"} label={`Waiting ${formatNumber(totalWaiting)}`} onClick={() => setScope("waiting")} />
            <ScopeToggle active={scope === "starved"} label={`Below fair ${formatNumber(totalStarved)}`} onClick={() => setScope("starved")} />
            <ScopeToggle active={scope === "all"} label="All" onClick={() => setScope("all")} />
          </div>
          <div className="relative w-full sm:w-auto">
            <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              type="search"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Search tenant, id, group"
              aria-label="Search tenants"
              className="h-8 w-full pl-8 text-xs sm:w-60"
            />
          </div>
          <Select
            value={groupFilter}
            onValueChange={setGroupFilter}
            aria-label="Filter tenants by group"
            className="h-8 w-40 text-xs"
            searchPlaceholder="Filter groups"
            options={[{ value: "all", label: "All groups" }, ...groups.map((g) => ({ value: g, label: g }))]}
          />
          <Select
            value={sort}
            onValueChange={(value) => setSort(value as TenantSort)}
            aria-label="Sort tenants"
            className="h-8 w-44 text-xs"
            options={[
              { value: "pressure", label: "Active pressure" },
              { value: "queued", label: "Queued" },
              { value: "deficit", label: "Slot deficit" },
              { value: "score", label: "Scheduler debt" },
              { value: "served", label: "Served work" },
              { value: "weight", label: "Weight" },
              { value: "share", label: "Weight share" },
            ]}
          />
        </div>
      </CardHeader>
      <CardContent className="p-0">
        <div className="grid items-start xl:grid-cols-[minmax(0,1.7fr)_minmax(18rem,1fr)]">
        <div className="min-w-0 overflow-x-auto">
          <table className="w-full min-w-[480px] text-sm">
            <thead>
              <tr className="border-b border-border text-left text-xs text-muted-foreground">
                <th className="px-6 py-3 font-medium">Tenant</th>
                <th className="px-3 py-3 text-right font-medium">Active / expected</th>
                <th className="px-3 py-3 text-right font-medium">Queued</th>
                <th className="px-6 py-3 text-right font-medium">Weight</th>
              </tr>
            </thead>
            <tbody>
              {shown.map((tenant) => (
                <TenantRow key={tenant.tenant_id} tenant={tenant} selected={selected?.tenant_id === tenant.tenant_id} onSelect={() => setSelectedId(tenant.tenant_id)} />
              ))}
              {shown.length === 0 && (
                <tr>
                  <td colSpan={4} className="px-6 py-12 text-center text-muted-foreground">
                    {view ? "No tenants match the current view" : "Waiting for tenant scheduler state"}
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
        <div className="p-4"><TenantInspector key={selected?.tenant_id ?? "empty"} tenant={selected} view={view} /></div>
        </div>
      </CardContent>
    </Card>
    </div>
  );
}

function TenantRow({ tenant, selected, onSelect }: { tenant: TenantFairshareView; selected: boolean; onSelect: () => void }) {
  const starved = isWaitingBelowShare(tenant);
  const color = colorForGroup(tenant.fairshare_group);

  return (
    <tr
      className={cn(
        "border-b border-border/60 transition-colors hover:bg-muted/20",
        selected && "bg-muted/30",
      )}
    >
      <td className="px-6 py-3">
        <div className="flex min-w-0 items-center gap-2">
          <span className="h-2.5 w-2.5 shrink-0 rounded-full" style={{ background: color }} />
          <div className="min-w-0">
            <div className="flex items-center gap-2">
              <button type="button" onClick={onSelect} aria-pressed={selected} className="text-left font-medium hover:underline">{tenant.name}</button>
              {starved && (
                <span className="rounded-sm bg-[hsl(350_65%_60%/0.16)] px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide text-[hsl(350_65%_64%)]">
                  Below share
                </span>
              )}
            </div>
            <p className="mt-0.5 text-xs text-muted-foreground">{tenant.fairshare_group}</p>
          </div>
        </div>
      </td>
      <td className="px-3 py-3 text-right tabular-nums">{formatNumber(tenant.in_flight)} / {formatDecimal(tenant.expected_slots)}</td>
      <td className="px-3 py-3 text-right tabular-nums">
        <span className={tenant.queued > 0 ? "text-[hsl(38_75%_62%)]" : ""}>{formatNumber(tenant.queued)}</span>
      </td>
      <td className="px-6 py-3 text-right tabular-nums">{formatNumber(tenant.weight)}</td>
    </tr>
  );
}



function WeightCell({ id, weight }: { id: string; weight: number }) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(weight);
  const [pending, start] = useTransition();
  const [error, setError] = useState<string>();
  const queryClient = useQueryClient();

  const commit = () => {
    if (pending) return;
    if (!Number.isSafeInteger(draft) || draft < 1) {
      setError("Enter a positive whole number.");
      return;
    }
    const next = draft;
    setError(undefined);
    if (next === weight) {
      setEditing(false);
      return;
    }
    start(async () => {
      try {
        await setWeightAction(id, next);
        await queryClient.invalidateQueries({ queryKey: ["fairshare-live"] });
        setEditing(false);
      } catch {
        setError("Could not save weight. Try again.");
      }
    });
  };

  if (!editing) {
    return (
      <button
        type="button"
        onClick={() => {
          setDraft(weight);
          setError(undefined);
          setEditing(true);
        }}
        className="group ml-auto flex items-center justify-end gap-1.5 tabular-nums text-muted-foreground transition-colors hover:text-foreground"
        aria-label="Edit tenant weight"
      >
        {formatNumber(weight)}
        <Pencil className="h-3 w-3 opacity-70" />
      </button>
    );
  }

  return (
    <div>
    <div className="flex flex-wrap items-center justify-end gap-1">
      <input
        type="number"
        min={1}
        autoFocus
        value={draft}
        disabled={pending}
        onChange={(e) => setDraft(Number(e.target.value))}
        onKeyDown={(e) => {
          if (e.key === "Enter") commit();
          if (e.key === "Escape" && !pending) setEditing(false);
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
    {error && <p role="alert" className="mt-2 text-xs text-destructive">{error}</p>}
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



function tenantDebt(_view: FairshareLiveView | undefined, tenant: TenantFairshareView): number {
  return tenant.served_tokens / Math.max(tenant.weight, 1);
}

function pressureStatus(utilization: number, queued: number): MetricTone {
  if (queued > 0 && utilization >= 90) return "hot";
  if (queued > 0 || utilization >= 75) return "warn";
  return "ok";
}
