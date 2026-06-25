"use client";

import { useQuery } from "@tanstack/react-query";
import type { ClusterResources } from "@/lib/obleth";

const EMPTY: ClusterResources = { partitions: [], nodes: [], accounts: [], qos: [] };

/**
 * Best-effort live view of the configured Slurm cluster's partitions, QoS, and
 * accounts, used to populate the placement field suggestions. Fetches the
 * `/api/live/slurm/resources` proxy; any failure (slurm disabled, version skew,
 * permission gap) leaves the fields as plain free-text inputs.
 *
 * Uses react-query (like the rest of the control plane) so the multiple
 * consumers on a page — the recipe gallery and the managed-model config form —
 * share a single cached request rather than each firing its own fetch.
 */
export function useClusterResources(): ClusterResources {
  const { data } = useQuery({
    queryKey: ["slurm", "resources"],
    queryFn: async (): Promise<ClusterResources> => {
      const r = await fetch("/api/live/slurm/resources");
      if (!r.ok) return EMPTY;
      const d = await r.json();
      if (!d || d.error) return EMPTY;
      return { ...EMPTY, ...d };
    },
    staleTime: 60_000,
    retry: false,
  });
  return data ?? EMPTY;
}
