import { describe, expect, it } from "vitest";
import { buildOverviewAttentionItems } from "./overview-attention";

describe("buildOverviewAttentionItems", () => {
  it("returns an all-clear list when no condition needs attention", () => {
    expect(buildOverviewAttentionItems({ unhealthyModels: 0, unknownModels: 0, queuedRequests: 0, waitingTenants: 0, starvedTenants: 0 })).toEqual([]);
  });

  it("routes model health conditions to Models", () => {
    const items = buildOverviewAttentionItems({ unhealthyModels: 1, unknownModels: 2, queuedRequests: 0, waitingTenants: 0, starvedTenants: 0 });
    expect(items.map((item) => item.href)).toEqual(["/models", "/models"]);
    expect(items[0].tone).toBe("hot");
  });

  it("elevates starved tenant queues and routes them to Fairshare", () => {
    const [item] = buildOverviewAttentionItems({ unhealthyModels: 0, unknownModels: 0, queuedRequests: 4, waitingTenants: 2, starvedTenants: 1 });
    expect(item).toMatchObject({ href: "/fairshare", tone: "hot" });
    expect(item.detail).toContain("below expected share");
  });
});
