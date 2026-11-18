import { beforeEach, describe, expect, it, vi } from "vitest";
import { obleth } from "@/lib/obleth";
import { loadModelOverview } from "./model-overview-data";

vi.mock("@/lib/obleth", () => ({
  obleth: {
    usageByModel: vi.fn(),
    usageSeriesByModel: vi.fn(),
    usageBreakdownByModel: vi.fn(),
    auditQuery: vi.fn(),
  },
}));

const DAY_MS = 86_400_000;
const NOW = 1_700_000_000_000;
const client = vi.mocked(obleth);

beforeEach(() => {
  vi.clearAllMocks();
  client.usageByModel.mockResolvedValue([{ model: "glm", requests: 40 }] as never);
  client.usageSeriesByModel.mockResolvedValue([
    { bucket_ms: NOW - 2 * DAY_MS + 1, requests: 7 },
    { bucket_ms: NOW - DAY_MS - 1, requests: 5 },
    { bucket_ms: NOW - DAY_MS + 1, requests: 40 },
  ] as never);
  client.usageBreakdownByModel.mockResolvedValue([{ tenant_id: "t1", tenant_name: "Lab", requests: 40, total_tokens: 9 }] as never);
  client.auditQuery.mockResolvedValue([{ id: 3, ts: "2026-01-01T00:00:00Z", actor: "ana", action: "update_model", entity_type: "model", entity_id: "m1", detail: {} }] as never);
});

describe("the model overview", () => {
  it("reads only its own model's usage and changes", async () => {
    const data = await loadModelOverview("m1", "glm", NOW);
    expect(client.usageByModel).toHaveBeenCalledTimes(1);
    expect(client.usageByModel).toHaveBeenCalledWith({ sinceMs: NOW - DAY_MS, model: "glm" });
    expect(client.auditQuery).toHaveBeenCalledWith({ entityType: "model", entityId: "m1", limit: 8 });
    expect(data.usage?.requests).toBe(40);
    // The day before comes from the two-day series the chart shows.
    expect(data.previousRequests).toBe(12);
    expect(data.tenants).toEqual([{ name: "Lab", requests: 40, tokens: 9 }]);
  });

  it("keeps a model's changes however many newer fleet-wide changes there are", async () => {
    // The audit read is scoped by entity, so a model last changed long ago
    // still lists its changes; nothing depends on a fleet-wide window.
    const data = await loadModelOverview("m1", "glm", NOW);
    expect(data.audit.map((e) => e.action)).toEqual(["update_model"]);
  });

  it("says the day before is unknown when the series could not be read", async () => {
    client.usageSeriesByModel.mockRejectedValue(new Error("down"));
    const data = await loadModelOverview("m1", "glm", NOW);
    expect(data.previousRequests).toBeNull();
    expect(data.series).toEqual([]);
  });
});
