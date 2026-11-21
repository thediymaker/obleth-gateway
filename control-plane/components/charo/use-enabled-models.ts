"use client";

import { useQuery } from "@tanstack/react-query";
import { modelRoutesQuery } from "@/lib/live-queries";
import type { ModelRoute } from "@/lib/obleth";

const NONE: ModelRoute[] = [];
const enabledOnly = (list: ModelRoute[]) => list.filter((m) => m.enabled);

/**
 * The enabled models, for the Playground and workflow cards that pick one. It
 * reads the shared model-routes query, so every component on the page shares
 * one request; a list read in the last 30 s is reused rather than read again.
 */
export function useEnabledModels(): { models: ModelRoute[]; loading: boolean; error: string | null; reload: () => void } {
  const query = useQuery({ ...modelRoutesQuery, select: enabledOnly, staleTime: 30_000 });
  return {
    models: query.data ?? NONE,
    loading: query.isPending,
    error: query.error ? `The models couldn't be loaded: ${query.error.message}` : null,
    reload: () => void query.refetch(),
  };
}
