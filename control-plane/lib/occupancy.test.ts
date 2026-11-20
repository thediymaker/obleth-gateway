import { describe, expect, it } from "vitest";
import type { CapacityDiscoveryView, FairshareLiveView, ModelRoute } from "@/lib/obleth";
import { buildDeploymentRows } from "@/lib/deployments-model";
import { buildModelRows } from "@/lib/models-model";
import { poolOccupancy } from "@/lib/occupancy";

// A Kubernetes-sized model with two endpoints: its configured cap (32) is not
// the pool in force (160 = 10 replicas × 16), which discovery reports.
const model = { id: "m2", model_name: "glm-5-3-flash", capacity_source: "kubernetes", max_in_flight: 32, enabled: true, model_type: "chat", endpoints: [{}, {}] } as unknown as ModelRoute;
const discovery = {
  models: [{ model_id: "m2", model_name: "glm-5-3-flash", enforced_max_in_flight: 160, in_flight: 12, cluster_in_flight: 38, status: { effective_max_in_flight: 160, ready_replicas: 10, per_replica_max_in_flight: 16 } }],
} as unknown as CapacityDiscoveryView;

describe("one pool reading for every page", () => {
  const cases: [string, FairshareLiveView | undefined, { inFlight: number; cap: number; queued: number }][] = [
    ["a pool the shared view lists", { mode: "shared", pools: [{ model: "glm-5-3-flash", in_flight: 12, cluster_in_flight: 38, cap: 80, configured_cap: 160, queued: 3 }] } as unknown as FairshareLiveView, { inFlight: 38, cap: 160, queued: 3 }],
    ["a pool a local view lists", { mode: "local", pools: [{ model: "glm-5-3-flash", in_flight: 12, cap: 160, queued: 1 }] } as unknown as FairshareLiveView, { inFlight: 12, cap: 160, queued: 1 }],
    ["a pool the view does not list", { mode: "shared", pools: [], model_queued: { "glm-5-3-flash": 2 } } as unknown as FairshareLiveView, { inFlight: 38, cap: 160, queued: 2 }],
  ];
  for (const [name, fairshare, want] of cases) {
    it(`Models and Deployments agree on ${name}`, () => {
      expect(poolOccupancy(model.model_name, model, fairshare, discovery)).toEqual(want);
      const [row] = buildModelRows([model], [], {}, [], fairshare, discovery);
      expect({ inFlight: row.inFlight, cap: row.cap, queued: row.queued }).toEqual(want);
      // The Deployments list and the deployment pages read each row's pool the same way.
      const [deployment] = buildDeploymentRows([model], [], [], discovery.models, {});
      expect(poolOccupancy(deployment.model.model_name, deployment.model, fairshare, discovery)).toEqual(want);
    });
  }

  it("falls back to the configured size, then the default", () => {
    expect(poolOccupancy("m", { ...model, model_name: "m" }, { model_in_flight: { m: 4 } } as unknown as FairshareLiveView)).toEqual({ inFlight: 4, cap: 32, queued: 0 });
    expect(poolOccupancy("m", { ...model, model_name: "m", max_in_flight: null }, { default_model_max_in_flight: 64 } as unknown as FairshareLiveView).cap).toBe(64);
  });

  it("gives a model that is off no pool", () => {
    expect(poolOccupancy(model.model_name, { ...model, enabled: false }, undefined, discovery)).toEqual({ inFlight: 0, cap: 0, queued: 0 });
  });
});
