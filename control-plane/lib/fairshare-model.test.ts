import { describe, expect, it } from "vitest";
import type { FairshareLiveView, KeyFairshareView, ModelRoute, TenantFairshareView } from "@/lib/obleth";
import { buildPoolRows, groupShares, keyRowLabel, nextInLine, poolState, poolTenantRows, previewTenantWeight, tenantPools, whyWaiting } from "./fairshare-model";

const tenant = (id: string, o: Partial<TenantFairshareView> = {}): TenantFairshareView => ({
  tenant_id: id, name: id, fairshare_group: "research", weight: 100, in_flight: 0, queued: 0,
  served_tokens: 0, share_score: 1, weight_share: 0, expected_slots: 0, ...o,
});
const route = (name: string, extra: Partial<ModelRoute> = {}) =>
  ({ id: name, model_name: name, upstream_model: name, api_base: "", enabled: true, max_in_flight: 8, ...extra }) as ModelRoute;

// The contended pool from the mockups: 64 slots, groups research 3 / teaching 2 / default 1.
const kimi = [
  tenant("cs-teaching", { fairshare_group: "teaching", in_flight: 4, queued: 11, expected_slots: 14.2, share_score: 0.2 }),
  tenant("library-ai", { fairshare_group: "teaching", weight: 50, in_flight: 1, queued: 4, expected_slots: 7.1, share_score: 0.3 }),
  tenant("openresearch", { in_flight: 16, queued: 8, expected_slots: 16, share_score: 0.9 }),
  tenant("voyager-prod", { fairshare_group: "default", in_flight: 10, expected_slots: 10.7, share_score: 0.5 }),
  tenant("research-computing", { in_flight: 33, expected_slots: 16, share_score: 2.1 }),
  tenant("idle", { expected_slots: 0 }),
];
const view = (tenants: TenantFairshareView[], extra: Partial<FairshareLiveView> = {}): FairshareLiveView => ({
  algorithm: "drr", max_in_flight: 64, global_in_flight: 64, global_queued: 23, groups: [], tenants,
  pools: [{ model: "kimi", cap: 64, in_flight: 64, queued: 23, borrowed: 17, groups: [], tenants, keys: [] }],
  ...extra,
});

describe("pools", () => {
  it("names a pool's state from its load", () => {
    expect(poolState(64, 64, 3)).toBe("full");
    expect(poolState(64, 64, 0)).toBe("busy");
    expect(poolState(52, 64, 0)).toBe("busy");
    expect(poolState(10, 64, 0)).toBe("normal");
    expect(poolState(0, 64, 0)).toBe("idle");
  });

  it("lists every enabled model, fullest first, from the snapshot or its configured size", () => {
    const rows = buildPoolRows(
      [route("idle"), route("kimi"), route("busy", { max_in_flight: 10 }), route("off", { enabled: false }), route("mock-model-x")],
      view(kimi, { model_in_flight: { busy: 9 } }),
    );
    expect(rows.map((r) => [r.model, r.state])).toEqual([["kimi", "full"], ["busy", "busy"], ["idle", "idle"]]);
    expect(rows[0]).toMatchObject({ inFlight: 64, cap: 64, queued: 23, tenants: 5 });
    expect(rows[1]).toMatchObject({ inFlight: 9, cap: 10, tenants: 0 });
  });

  it("marks a model with no servers as such and lists it after the idle pools", () => {
    const rows = buildPoolRows(
      [route("unplaced"), route("idle"), route("kimi")],
      view(kimi, { models_without_servers: ["unplaced"] }),
    );
    expect(rows.map((r) => [r.model, r.state])).toEqual([["kimi", "full"], ["idle", "idle"], ["unplaced", "no_servers"]]);
  });
});

describe("tenants in a pool", () => {
  it("puts those waiting below their share first, in the order freed slots reach them", () => {
    const rows = poolTenantRows(kimi);
    expect(rows.map((r) => [r.tenant.name, r.standing, r.place])).toEqual([
      ["cs-teaching", "next", 1],
      ["library-ai", "next", 2],
      ["openresearch", "waiting", 3],
      ["research-computing", "above", null],
      ["voyager-prod", "within", null],
    ]);
    expect(rows.find((r) => r.tenant.name === "research-computing")?.above).toBe(17);
    expect(nextInLine(kimi).map((t) => t.name)).toEqual(["cs-teaching", "library-ai", "openresearch"]);
  });

  it("explains a wait by who holds slots above their share", () => {
    const rows = poolTenantRows(kimi);
    const why = whyWaiting(rows[0], rows);
    expect(why.holders.map((h) => [h.tenant.name, h.above])).toEqual([["research-computing", 17]]);
    expect(why.place).toBe(1);
    expect(why.line).toHaveLength(3);
  });

  it("counts the pools a tenant has work in and its busiest", () => {
    const v = view(kimi);
    v.pools!.push({ model: "qwen", cap: 48, in_flight: 2, queued: 0, borrowed: 0, groups: [], keys: [], tenants: [tenant("cs-teaching", { in_flight: 2 })] });
    expect(tenantPools(v, "cs-teaching")).toEqual({ count: 2, top: "kimi" });
    expect(tenantPools(v, "idle")).toEqual({ count: 0, top: null });
  });
});

describe("weight previews", () => {
  it("moves a tenant's share only inside its group, scaled from the gateway's own figure", () => {
    const [p] = previewTenantWeight(view(kimi), "cs-teaching", 200);
    // Teaching's 21.3 slots split 200:50 instead of 100:50.
    expect(p.model).toBe("kimi");
    expect(p.from).toBeCloseTo(14.2);
    expect(p.to).toBeCloseTo(14.2 * (200 / 250) / (100 / 150));
    expect(p.groupPeers).toEqual(["library-ai"]);
  });

  it("changes nothing where the tenant is its group's only active one", () => {
    const [p] = previewTenantWeight(view(kimi), "voyager-prod", 400);
    expect(p.to).toBeCloseTo(p.from);
    expect(p.groupPeers).toEqual([]);
  });

  it("skips pools where the tenant has no work", () => {
    expect(previewTenantWeight(view(kimi), "idle", 500)).toEqual([]);
  });

  it("shares a full pool between the groups with work, by weight", () => {
    const shares = groupShares([
      { name: "research", weight: 3, active: true },
      { name: "teaching", weight: 3, active: true },
      { name: "default", weight: 1, active: true },
      { name: "quiet", weight: 5, active: false },
    ]);
    expect(shares.map((s) => Math.round(s.share * 100))).toEqual([43, 43, 14, 0]);
    // With nothing active anywhere, every group counts.
    expect(groupShares([{ name: "a", weight: 1, active: false }, { name: "b", weight: 3, active: false }]).map((s) => s.share)).toEqual([0.25, 0.75]);
  });
});

describe("keyRowLabel", () => {
  const row = (over: Partial<KeyFairshareView> = {}): KeyFairshareView => ({
    key_id: "0f6c2d8e-0000-4000-8000-000000000000", tenant_id: "t", name: "createai-builder", weight: 100,
    max_in_flight: null, in_flight: 1, queued: 0, served_tokens: 0, share_score: 0, weight_share: 0, expected_slots: 0, ...over,
  });

  it("names a key by its name, or its id when it has none", () => {
    expect(keyRowLabel(row())).toBe("createai-builder");
    expect(keyRowLabel(row({ name: "" }))).toBe("0f6c2d8e");
  });

  it("names an end user's row after the key and the user the app named", () => {
    expect(keyRowLabel(row({ end_user: "asurite-jdoe", parent_key_id: "k" }))).toBe("createai-builder · asurite-jdoe");
  });
});
