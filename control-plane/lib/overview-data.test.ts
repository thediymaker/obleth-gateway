import { beforeEach, describe, expect, it, vi } from "vitest";
import { obleth } from "@/lib/obleth";
import { loadOverviewWindow, TOP_TENANTS } from "./overview-data";
import { RANGE_BUCKET_MS, RANGE_MS } from "./overview-model";

vi.mock("@/lib/obleth", () => ({
  obleth: { overview: vi.fn() },
}));

const NOW = 1_700_000_000_000;
const client = vi.mocked(obleth);

const summary = (requests: number) => ({
  requests,
  tokens: requests * 10,
  cost: 1,
  has_pricing: true,
  tenant_count: 9,
  active_tenants: 7,
  model_count: 3,
  enabled_models: 2,
  key_count: 4,
});

beforeEach(() => {
  vi.clearAllMocks();
  client.overview.mockResolvedValue({
    since_ms: NOW - RANGE_MS["24h"],
    until_ms: NOW,
    previous_since_ms: NOW - 2 * RANGE_MS["24h"],
    bucket_ms: RANGE_BUCKET_MS["24h"],
    summary: summary(100),
    previous: summary(80),
    series: [],
    models: [],
    tenants: Array.from({ length: TOP_TENANTS + 2 }, (_, i) => ({
      tenant_id: `t${i}`,
      name: i === 0 ? null : `Tenant ${i}`,
      requests: 100 - i,
      total_tokens: 10,
      cost_usd: 0.5,
    })),
    cache: { hits: 1, misses: 2, tokens_saved: 3 },
  } as never);
});

describe("the Overview window", () => {
  it("comes from one read and folds tenants past the top rows into one line", async () => {
    const win = await loadOverviewWindow("24h", NOW);
    expect(client.overview).toHaveBeenCalledTimes(1);
    expect(client.overview).toHaveBeenCalledWith(NOW - RANGE_MS["24h"], RANGE_BUCKET_MS["24h"]);
    expect(win.summary.requests).toBe(100);
    expect(win.previous?.requests).toBe(80);
    expect(win.tenants.map((t) => t.name)).toEqual(["Deleted tenant", "Tenant 1", "Tenant 2", "Tenant 3", "Tenant 4"]);
    expect(win.otherTenants).toEqual({ count: 2, requests: 95 + 94, tokens: 20, cost: 1 });
    expect(win.cache).toEqual({ hits: 1, misses: 2, tokens_saved: 3 });
  });

  it("reads as empty when the gateway cannot answer", async () => {
    client.overview.mockRejectedValue(new Error("down"));
    const win = await loadOverviewWindow("24h", NOW);
    expect(win.summary.requests).toBe(0);
    expect(win.previous).toBeNull();
    expect(win.tenants).toEqual([]);
  });
});
