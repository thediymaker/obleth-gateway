"use client";

import { useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { RefreshCw } from "lucide-react";
import type { CapacityDiscoveryView, FairshareLiveView, LiveStats, ModelHealthSummary, ModelRoute } from "@/lib/obleth";
import type { OverviewContext, OverviewWindow } from "@/lib/overview-data";
import {
  PREVIOUS_LABEL,
  buildAttention,
  buildFleet,
  buildSeries,
  changeLabel,
  compact,
  formatMs,
  type OverviewRange,
} from "@/lib/overview-model";
import { isWaitingBelowShare } from "@/lib/fairshare";
import { cn, getJson } from "@/lib/utils";
import { Fleet, RightNow, type LiveCapacity } from "@/components/overview/fleet";
import { FooterTiles, NeedsAttention, RecentChanges, TopTenants } from "@/components/overview/panels";
import { ModelPeek } from "@/components/overview/model-peek";
import { TrafficPanel } from "@/components/overview/traffic-panel";
import { HealthGlyph, Kpi, Pill, Segmented } from "@/components/overview/ui";

const FAST_POLL_MS = 2_000;
const STATS_POLL_MS = 5_000;
const HEALTH_POLL_MS = 30_000;
const SLOW_POLL_MS = 60_000;
const WINDOW_POLL_MS: Record<OverviewRange, number> = { "1h": 15_000, "24h": 30_000, "7d": 120_000 };

/** Live capacity: cluster-wide with shared slots, otherwise the answering gateway's own. */
function liveCapacity(view?: FairshareLiveView, stats?: LiveStats): LiveCapacity {
  const shared = (view?.mode ?? stats?.mode) === "shared";
  const cluster = shared ? view?.cluster_in_flight ?? stats?.cluster_in_flight ?? null : null;
  return {
    inFlight: cluster ?? view?.global_in_flight ?? stats?.in_flight ?? 0,
    slots: (shared ? view?.configured_max_in_flight : undefined) ?? view?.max_in_flight ?? stats?.max_in_flight ?? 0,
    queued: view?.global_queued ?? stats?.queued ?? 0,
    replicas: view?.replicas ?? stats?.replicas ?? 1,
    shared,
    activeTenants: view?.tenants.filter((t) => t.in_flight > 0 || t.queued > 0).length ?? 0,
    belowShare: view?.tenants.filter(isWaitingBelowShare).length ?? 0,
  };
}

function money(v: number) {
  return `$${v >= 1000 ? compact(v) : v.toFixed(2)}`;
}

export function OverviewDashboard({
  initialModels,
  initialWindow,
  initialContext,
  initialHealth,
  initialFairshare,
  initialStats,
}: {
  initialModels: ModelRoute[];
  initialWindow: OverviewWindow;
  initialContext: OverviewContext;
  initialHealth: ModelHealthSummary[];
  initialFairshare?: FairshareLiveView;
  initialStats?: LiveStats;
}) {
  const queryClient = useQueryClient();
  const [range, setRange] = useState<OverviewRange>(initialWindow.range);
  const [peek, setPeek] = useState<string | null>(null);

  const windowQuery = useQuery({
    queryKey: ["overview-window", range],
    queryFn: () => getJson<OverviewWindow>(`/api/live/overview?range=${range}`),
    initialData: range === initialWindow.range ? initialWindow : undefined,
    placeholderData: (previous) => previous,
    refetchInterval: WINDOW_POLL_MS[range],
  });
  const contextQuery = useQuery({ queryKey: ["overview-context"], queryFn: () => getJson<OverviewContext>("/api/live/overview/context"), initialData: initialContext, refetchInterval: SLOW_POLL_MS });
  const fairshareQuery = useQuery({ queryKey: ["fairshare-live"], queryFn: () => getJson<FairshareLiveView>("/api/live/fairshare"), initialData: initialFairshare, refetchInterval: FAST_POLL_MS });
  const statsQuery = useQuery({ queryKey: ["gateway-stats"], queryFn: () => getJson<LiveStats>("/api/live/stats"), initialData: initialStats, refetchInterval: STATS_POLL_MS });
  const modelsQuery = useQuery({ queryKey: ["model-routes"], queryFn: () => getJson<ModelRoute[]>("/api/live/models"), initialData: initialModels, refetchInterval: SLOW_POLL_MS });
  // Pool sizes and cluster-wide counts for models the answering gateway has not served yet.
  const discoveryQuery = useQuery({ queryKey: ["capacity-discovery"], queryFn: () => getJson<CapacityDiscoveryView>("/api/live/capacity/discovery"), refetchInterval: STATS_POLL_MS, retry: false });
  const healthQuery = useQuery({ queryKey: ["model-health"], queryFn: () => getJson<ModelHealthSummary[]>("/api/live/models/health"), initialData: initialHealth, refetchInterval: HEALTH_POLL_MS });

  const win = windowQuery.data ?? initialWindow;
  const context = contextQuery.data ?? initialContext;
  const fairshare = fairshareQuery.data;
  const models = modelsQuery.data ?? initialModels;
  const health = healthQuery.data ?? initialHealth;

  const series = useMemo(() => buildSeries(win.series, win.range, win.now), [win]);
  const discovery = discoveryQuery.data;
  const fleet = useMemo(() => buildFleet(models, health, fairshare, win.models, Date.now(), discovery), [models, health, fairshare, win.models, discovery]);
  const { items, watching } = useMemo(() => buildAttention(fleet, fairshare), [fleet, fairshare]);
  const capacity = useMemo(() => liveCapacity(fairshare, statsQuery.data), [fairshare, statsQuery.data]);
  const liveTenants = useMemo(() => new Map((fairshare?.tenants ?? []).map((t) => [t.tenant_id, t])), [fairshare]);
  const modelNames = useMemo(() => new Map(models.map((m) => [m.id, m.model_name])), [models]);
  const tenantNames = useMemo(() => new Map(Object.entries(context.tenantNames)), [context.tenantNames]);

  const s = win.summary;
  const prev = win.previous;
  const against = PREVIOUS_LABEL[win.range];
  const hasErrors = s.detailed;
  const errorRate = s.requests ? (s.errors / s.requests) * 100 : 0;
  const prevErrorRate = prev?.requests ? (prev.errors / prev.requests) * 100 : 0;
  const errorDetail = !hasErrors ? "Needs a newer gateway" : `${s.errors.toLocaleString()} failed${prev && prevErrorRate ? ` · was ${prevErrorRate.toFixed(prevErrorRate < 1 ? 2 : 1)}%` : ""}`;
  const serving = fleet.filter((t) => t.health === "healthy" || t.health === "unknown").length;
  const pct = (v: number) => `${v.toFixed(v < 1 ? 2 : 1)}%`;
  const updatedAt = Math.max(windowQuery.dataUpdatedAt, fairshareQuery.dataUpdatedAt);
  const fetching = windowQuery.isFetching || contextQuery.isFetching;

  function refreshAll() {
    for (const key of ["overview-window", "overview-context", "fairshare-live", "gateway-stats", "model-routes", "model-health", "capacity-discovery"]) {
      void queryClient.invalidateQueries({ queryKey: [key] });
    }
  }

  const peekTile = fleet.find((t) => t.name === peek);

  return (
    <div className="mx-auto flex max-w-[1600px] flex-col gap-5">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="flex min-w-0 flex-col gap-1.5">
          <h1 className="text-[26px] font-semibold tracking-tight">Gateway overview</h1>
          <div className="flex flex-wrap items-center gap-2.5 text-[13.5px] text-secondary-foreground" role="status">
            {items.length > 0
              ? <Pill inverted>{items.length} need{items.length === 1 ? "s" : ""} attention</Pill>
              : <><HealthGlyph state="healthy" className="h-[9px] w-[9px]" /><strong className="font-semibold text-foreground">All systems normal</strong></>}
            <span>
              {serving} of {fleet.length} models serving · {capacity.inFlight.toLocaleString()} in flight · {capacity.queued ? <strong className="font-semibold text-foreground">{capacity.queued.toLocaleString()} queued</strong> : "nothing queued"}
            </span>
          </div>
        </div>
        <div className="flex items-center gap-2.5">
          <span className="text-xs text-muted-foreground">{updatedAt ? `Updated ${new Date(updatedAt).toLocaleTimeString()}` : ""}</span>
          <Segmented label="Time range" value={range} onChange={setRange} options={[{ value: "1h", label: "1h" }, { value: "24h", label: "24h" }, { value: "7d", label: "7d" }]} />
          <button type="button" onClick={refreshAll} aria-label="Refresh" title="Refresh" className="flex h-[34px] w-[34px] items-center justify-center rounded-lg border border-border text-muted-foreground hover:bg-accent hover:text-foreground">
            <RefreshCw className={cn("h-3.5 w-3.5", fetching && "animate-spin")} />
          </button>
        </div>
      </div>

      <NeedsAttention items={items} watching={watching} />

      <div className={cn("grid grid-cols-2 gap-3 md:grid-cols-3 xl:grid-cols-5", windowQuery.isPlaceholderData && "opacity-60")}>
        <Kpi label={`Requests · ${win.range}`} value={compact(s.requests)} detail={prev ? changeLabel(s.requests, prev.requests, against) : null} trend={series.map((p) => p.requests)} href="/reports" />
        <Kpi label={`Tokens · ${win.range}`} value={compact(s.tokens)} detail={s.inputTokens || s.outputTokens ? `${compact(s.inputTokens)} in · ${compact(s.outputTokens)} out` : prev ? changeLabel(s.tokens, prev.tokens, against) : null} trend={series.map((p) => p.tokens)} href="/reports" />
        <Kpi label={`Error rate · ${win.range}`} value={hasErrors ? pct(errorRate) : "—"} detail={errorDetail} trend={series.map((p) => p.errors)} href="/logs" emphasis={hasErrors && errorRate >= 2 && errorRate > prevErrorRate * 2} />
        <Kpi label="First token · p50" value={formatMs(s.p50TtftMs)} detail={s.avgTtftMs ? `avg ${formatMs(s.avgTtftMs)}` : null} trend={series.map((p) => p.ttft ?? 0)} href="/reports" />
        <Kpi label={`Spend · ${win.range}`} value={s.hasPricing ? money(s.cost) : "—"} detail={s.hasPricing ? `at list price · ${s.activeTenants} tenant${s.activeTenants === 1 ? "" : "s"}` : "No prices set on models"} trend={series.map((p) => p.cost)} href="/reports" />
      </div>

      <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_400px]">
        <TrafficPanel series={series} range={win.range} hasErrors={hasErrors} />
        <RightNow capacity={capacity} fleet={fleet} onOpen={setPeek} />
      </div>

      <Fleet fleet={fleet} range={win.range} onOpen={setPeek} />

      <div className="grid gap-4 lg:grid-cols-2">
        <TopTenants window={win} live={liveTenants} />
        <RecentChanges audit={context.audit} models={modelNames} tenants={tenantNames} />
      </div>

      <FooterTiles summary={s} cache={win.cache} readiness={context.readiness} range={win.range} />

      <ModelPeek
        tile={peekTile}
        model={models.find((m) => m.model_name === peek)}
        usage={win.models.find((u) => u.model === peek)}
        audit={context.audit}
        range={win.range}
        onClose={() => setPeek(null)}
      />
    </div>
  );
}
