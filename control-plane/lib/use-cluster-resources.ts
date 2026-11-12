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
      const r = await fetch("/api/live/slurm/resources").catch(() => null);
      if (!r) return { ...EMPTY, error: "obleth couldn't reach the gateway to read the cluster." };
      const d = await r.json().catch(() => null);
      // A newer gateway sends what it could read plus a sentence about what it
      // couldn't; an error from the proxy itself carries no lists.
      if (!r.ok || !d || !Array.isArray(d.partitions)) {
        const why = d && typeof d.error === "string" ? d.error : `the gateway answered ${r.status}`;
        return { ...EMPTY, error: `obleth couldn't read the cluster: ${why}` };
      }
      return { ...EMPTY, ...d };
    },
    staleTime: 60_000,
    retry: false,
  });
  return data ?? EMPTY;
}
