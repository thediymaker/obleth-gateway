"use client";

import { useEffect, useState } from "react";
import type { ClusterResources } from "@/lib/obleth";

const EMPTY: ClusterResources = { partitions: [], nodes: [], accounts: [], qos: [] };

/**
 * Best-effort live view of the configured Slurm cluster's partitions, QoS, and
 * accounts, used to populate the placement field suggestions. Fetches the
 * `/api/live/slurm/resources` proxy once on mount; any failure (slurm disabled,
 * version skew, permission gap) leaves the fields as plain free-text inputs.
 */
export function useClusterResources(): ClusterResources {
  const [data, setData] = useState<ClusterResources>(EMPTY);
  useEffect(() => {
    let active = true;
    fetch("/api/live/slurm/resources")
      .then((r) => (r.ok ? r.json() : null))
      .then((d) => {
        if (active && d && !d.error) setData({ ...EMPTY, ...d });
      })
      .catch(() => {
        /* keep the empty fallback — fields stay free-text */
      });
    return () => {
      active = false;
    };
  }, []);
  return data;
}
