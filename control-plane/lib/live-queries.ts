// The /api/live reads that several pages share, each under one query key so
// react-query holds one copy and sends one request for every component that
// asks for it. Spread one into useQuery and add the page's own polling.
import type { CapacityDiscoveryView, FairshareLiveView, ModelRoute } from "@/lib/obleth";
import { getJson } from "@/lib/utils";

export const fairshareLiveQuery = {
  queryKey: ["fairshare-live"],
  queryFn: () => getJson<FairshareLiveView>("/api/live/fairshare"),
} as const;

export const modelRoutesQuery = {
  queryKey: ["model-routes"],
  queryFn: () => getJson<ModelRoute[]>("/api/live/models"),
} as const;

export const capacityDiscoveryQuery = {
  queryKey: ["capacity-discovery"],
  queryFn: () => getJson<CapacityDiscoveryView>("/api/live/capacity/discovery"),
} as const;
