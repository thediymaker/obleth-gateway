import { describe, expect, it } from "vitest";
import type { FairshareLiveView, ModelPoolView, TenantFairshareView } from "@/lib/obleth";
import { backgroundRoom, belowShareInPools, countsScope, isBackgroundPool, isWaitingBelowShare, tenantsAboveShare, tenantsBelowShare } from "./fairshare";

const tenant = (id: string, o: Partial<TenantFairshareView> = {}): TenantFairshareView => ({
  tenant_id: id, name: id, fairshare_group: "default", weight: 100, in_flight: 0, queued: 0,
  served_tokens: 0, share_score: 0, weight_share: 0, expected_slots: 0, ...o,
});
const pool = (model: string, cap: number, tenants: TenantFairshareView[]): ModelPoolView => ({
  model, cap, configured_cap: cap, in_flight: tenants.reduce((n, t) => n + t.in_flight, 0),
  queued: tenants.reduce((n, t) => n + t.queued, 0), borrowed: 0, groups: [], tenants, keys: [],
});
const view = (pools: ModelPoolView[], extra: Partial<FairshareLiveView> = {}): FairshareLiveView => ({
  algorithm: "hierarchical", max_in_flight: 0, global_in_flight: 0, global_queued: 0, groups: [],
  tenants: [], pools, ...extra,
});

describe("isWaitingBelowShare", () => {
  const borrower = { tenant_id: "b", queued: 0, in_flight: 7, expected_slots: 5 };
  const row = (o: { queued: number; in_flight: number; expected_slots: number }) => ({ tenant_id: "t", ...o });

  it("needs a queue and fewer slots than the whole slots of its share", () => {
    const short = row({ queued: 2, in_flight: 1, expected_slots: 3.6 });
    expect(isWaitingBelowShare(short, [short, borrower])).toBe(true);
    const atShare = row({ queued: 2, in_flight: 3, expected_slots: 3.6 });
    expect(isWaitingBelowShare(atShare, [atShare, borrower])).toBe(false);
    const idle = row({ queued: 0, in_flight: 0, expected_slots: 3.6 });
    expect(isWaitingBelowShare(idle, [idle, borrower])).toBe(false);
  });

  it("needs another tenant in the pool holding more than its share", () => {
    const short = row({ queued: 2, in_flight: 1, expected_slots: 3.6 });
    expect(isWaitingBelowShare(short, [short])).toBe(false);
    const fair = { tenant_id: "f", queued: 1, in_flight: 2, expected_slots: 2.4 };
    expect(isWaitingBelowShare(short, [short, fair])).toBe(false);
  });

  it("does not flag a lone tenant whose cluster counts read a few slots short of a full pool", () => {
    // Counts gathered from several gateways can lag by a moment, so a tenant
    // holding every slot can read 250 of 256 while it waits.
    const lone = row({ queued: 500, in_flight: 250, expected_slots: 256 });
    expect(isWaitingBelowShare(lone, [lone])).toBe(false);
  });
});

describe("below share, judged per pool", () => {
  // One tenant holds every slot of a full pool and has requests queued,
  // counted across every gateway.
  const full = pool("big", 256, [tenant("acme", { in_flight: 256, queued: 767, weight_share: 1, expected_slots: 256 })]);

  it("does not flag a tenant holding its whole share of a full pool", () => {
    expect(belowShareInPools(view([full]))).toEqual([]);
  });

  it("flags the pool a tenant is short in, even when it is over its share in another", () => {
    const busy = pool("a", 10, [tenant("t", { in_flight: 10, expected_slots: 5 }), tenant("u", { in_flight: 0, queued: 1, expected_slots: 5 })]);
    const short = pool("b", 10, [tenant("t", { in_flight: 1, queued: 4, expected_slots: 5 }), tenant("v", { in_flight: 9, expected_slots: 5 })]);
    // Added up across pools, t holds 11 against 10 and would read as fine.
    const v = view([busy, short], { tenants: [tenant("t", { in_flight: 11, queued: 4, expected_slots: 10 })] });
    expect(belowShareInPools(v).map((b) => [b.model, b.tenant.tenant_id])).toEqual([["a", "u"], ["b", "t"]]);
    expect([...tenantsBelowShare(v)].sort()).toEqual(["t", "u"]);
    expect([...tenantsAboveShare(v)].sort()).toEqual(["t", "v"]);
  });

  it("falls back to the aggregated tenants for a view with no per-pool detail", () => {
    const v = view([], { pools: undefined, tenants: [tenant("t", { in_flight: 1, queued: 2, expected_slots: 3.6 }), tenant("b", { in_flight: 6, expected_slots: 3.6 })] });
    expect(belowShareInPools(v)).toEqual([{ model: null, tenant: v.tenants[0] }]);
  });

  it("is empty without a view", () => {
    expect(belowShareInPools(undefined)).toEqual([]);
    expect(tenantsBelowShare(undefined).size).toBe(0);
  });

  it("leaves background pools out on request: waiting there is by design", () => {
    const bg = pool("m (background)", 192, [tenant("batch", { in_flight: 187, expected_slots: 96 }), tenant("bench", { in_flight: 4, queued: 2, expected_slots: 96 })]);
    const v = view([bg]);
    expect(belowShareInPools(v).map((b) => b.tenant.tenant_id)).toEqual(["bench"]);
    expect(belowShareInPools(v, { background: false })).toEqual([]);
    expect(tenantsBelowShare(v, { background: false }).size).toBe(0);
  });
});

describe("background pools", () => {
  it("are told apart by name", () => {
    expect(isBackgroundPool("m (background)")).toBe(true);
    expect(isBackgroundPool("m")).toBe(false);
    expect(isBackgroundPool(null)).toBe(false);
  });

  it("report their room against the model's whole pool", () => {
    const v = view([pool("m", 256, []), pool("m (background)", 192, [])], { mode: "shared" });
    expect(backgroundRoom(v, "m (background)")).toEqual({ cap: 192, modelCap: 256 });
    expect(backgroundRoom(view([pool("m (background)", 192, [])]), "m (background)")).toEqual({ cap: 192, modelCap: null });
    expect(backgroundRoom(v, "m")).toBeNull();
  });
});

describe("countsScope", () => {
  it("reads whose counts a view holds", () => {
    expect(countsScope(view([], { scope: "cluster", replicas: 3, mode: "shared" }))).toBe("cluster");
    expect(countsScope(view([], { scope: "replica", replicas: 3, mode: "shared" }))).toBe("gateway");
    expect(countsScope(view([], { replicas: 2, mode: "split" }))).toBe("gateway");
    expect(countsScope(view([], { replicas: 1, mode: "local" }))).toBe("single");
    expect(countsScope(undefined)).toBe("single");
  });
});
