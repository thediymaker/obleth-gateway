"use client";
import { useQuery } from "@tanstack/react-query";
import type { ClusterResources } from "@/lib/obleth";

export function useClusterResources() {
  return useQuery({
    queryKey: ["slurm-resources"],
    queryFn: async (): Promise<ClusterResources> => {
      const r = await fetch("/api/live/slurm/resources");
      if (!r.ok) throw new Error("discovery failed");
      return r.json();
    },
    staleTime: 60_000,
    retry: false, // discovery is best-effort; fall back to free-text on failure
  });
}
