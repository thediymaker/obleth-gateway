"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import type { CapacityDiscoveryView, FairshareHistoryPoint, FairshareLiveView, ModelRoute } from "@/lib/obleth";
import { appendHistoryTail, buildHistoryChart, fetchHistory } from "@/lib/fairshare-model";

export const FAIRSHARE_POLL_MS = 2000;
const ROUTES_POLL_MS = 30_000;
const DISCOVERY_POLL_MS = 5_000;
const THROUGHPUT_POLL_MS = 15_000;

export function useFairshareLive() {
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

export function useModelRoutes() {
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

/** Pool sizes and cluster-wide counts for models the answering gateway has not served yet. */
export function useCapacityDiscovery() {
  return useQuery({
    queryKey: ["capacity-discovery"],
    queryFn: async () => {
      const res = await fetch("/api/live/capacity/discovery");
      if (!res.ok) throw new Error("capacity discovery unavailable");
      return (await res.json()) as CapacityDiscoveryView;
    },
    refetchInterval: DISCOVERY_POLL_MS,
    retry: false,
  });
}

export interface TenantSeriesRow {
  bucket_ms: number;
  tenant_id: string;
  requests: number;
  total_tokens: number;
}

/** Per-tenant requests over the last 30 minutes in 30-second buckets, for the tenant panel's trend. */
export function useTenantSeries(enabled: boolean) {
  return useQuery({
    queryKey: ["usage-series-tenants"],
    queryFn: async () => {
      const since = Date.now() - 1_800_000;
      const res = await fetch(`/api/live/usage/tenants?bucket_ms=30000&since_ms=${since}`);
      if (!res.ok) throw new Error("usage series unavailable");
      return (await res.json()) as TenantSeriesRow[];
    },
    refetchInterval: THROUGHPUT_POLL_MS,
    enabled,
  });
}

/** Retained scheduler history for one scope ("all" or a model name): the
 *  full window on mount and scope change, then the tail every poll. */
export function useFairshareHistory(scope: string) {
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

  // "History since" must not claim a time earlier than the first plotted
  // point, nor hide a point the ring already reports as retained.
  const displayOldestTs = useMemo(() => {
    const firstPointTs = points.length ? points[0].ts_ms : null;
    if (oldestTs !== null && firstPointTs !== null) return Math.min(oldestTs, firstPointTs);
    return oldestTs ?? firstPointTs;
  }, [oldestTs, points]);

  const chart = useMemo(() => buildHistoryChart(points), [points]);
  return { ...chart, oldestTs: displayOldestTs, retentionMs };
}
