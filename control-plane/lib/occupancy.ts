import type { CapacityDiscoveryView, FairshareLiveView, ModelRoute } from "@/lib/obleth";

/** A model's pool right now: requests running, its size, and requests waiting. */
export interface Occupancy {
  inFlight: number;
  cap: number;
  queued: number;
}

/**
 * Live pool occupancy for one model: cluster-wide in shared mode, else this
 * gateway's own. Waiting follows the view's scope: every gateway's when the
 * view is cluster-wide. The view lists only the pools with traffic somewhere
 * or that the answering gateway has served since it started, so a model it
 * does not list falls back to capacity discovery (its enforced size and
 * cluster-wide count), then to the configured size. A model that is off has
 * no pool. Every page that shows a model's pool reads it here, so Models,
 * Deployments and the model and deployment pages agree.
 */
export function poolOccupancy(name: string, model: ModelRoute, fairshare?: FairshareLiveView, discovery?: CapacityDiscoveryView | null): Occupancy {
  return occupancyOf(name, model, fairshare, discoveredPool(name, discovery));
}

/** The pool capacity discovery enforces for a model, when it does. */
export interface DiscoveredPool {
  inFlight: number;
  cap: number;
}

/** Just the numbers {@link poolOccupancy} reads from capacity discovery, so a
 *  live cell can watch them without the rest of the view. */
export function discoveredPool(name: string, discovery?: CapacityDiscoveryView | null): DiscoveredPool | null {
  const found = discovery?.models.find((d) => d.model_name === name && d.enforced_max_in_flight > 0);
  return found ? { inFlight: found.cluster_in_flight ?? found.in_flight, cap: found.enforced_max_in_flight } : null;
}

/** {@link poolOccupancy} with the discovered pool already picked out. */
export function occupancyOf(name: string, model: ModelRoute, fairshare: FairshareLiveView | undefined, discovered: DiscoveredPool | null): Occupancy {
  if (!model.enabled) return { inFlight: 0, cap: 0, queued: 0 };
  const shared = fairshare?.mode === "shared";
  const pool = fairshare?.pools?.find((p) => p.model === name);
  if (pool) {
    return {
      inFlight: shared ? pool.cluster_in_flight ?? pool.in_flight : pool.in_flight,
      cap: shared ? pool.configured_cap ?? pool.cap : pool.cap,
      queued: pool.queued,
    };
  }
  if (discovered) return { ...discovered, queued: fairshare?.model_queued?.[name] ?? 0 };
  return {
    inFlight: fairshare?.model_in_flight?.[name] ?? 0,
    cap: model.max_in_flight ?? fairshare?.default_model_max_in_flight ?? 0,
    queued: fairshare?.model_queued?.[name] ?? 0,
  };
}
