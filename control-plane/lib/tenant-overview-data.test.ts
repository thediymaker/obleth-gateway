import { beforeEach, describe, expect, it, vi } from "vitest";
import { obleth } from "@/lib/obleth";
import { loadTenantOverview } from "./tenant-overview-data";

vi.mock("@/lib/obleth", () => ({
  obleth: {
    usage: vi.fn(),
    usageLogHistogram: vi.fn(),
    usageLogFacets: vi.fn(),
    usageDaily: vi.fn(),
    usageKeysSummary: vi.fn(),
    auditQuery: vi.fn(),
  },
}));

const DAY_MS = 86_400_000;
const NOW = 1_700_000_000_000;
const client = vi.mocked(obleth);

beforeEach(() => {
  vi.clearAllMocks();
  client.usage.mockImplementation(async ({ sinceMs }) => [{ tenant_id: "t1", requests: sinceMs === NOW - DAY_MS ? 10 : 25 }] as never);
  client.usageLogHistogram.mockResolvedValue({ buckets: [{ errors: 2 }] } as never);
  client.usageLogFacets.mockResolvedValue({ failures: [] } as never);
  client.usageDaily.mockResolvedValue([] as never);
  client.usageKeysSummary.mockResolvedValue([] as never);
  client.auditQuery.mockResolvedValue([{ id: 9, ts: "2026-01-01T00:00:00Z", actor: "ana", action: "update_tenant", entity_type: "tenant", entity_id: "t1", detail: {} }] as never);
});

describe("the tenant overview", () => {
  it("reads only its own tenant's usage and changes", async () => {
    const data = await loadTenantOverview("t1", NOW);
    expect(client.usage).toHaveBeenCalledWith({ sinceMs: NOW - DAY_MS, tenantId: "t1" });
    expect(client.usage).toHaveBeenCalledWith({ sinceMs: NOW - 2 * DAY_MS, tenantId: "t1" });
    expect(client.auditQuery).toHaveBeenCalledWith({ entityType: "tenant", entityId: "t1", limit: 10 });
    expect(data.requests24h).toBe(10);
    expect(data.previous24h).toBe(15);
    expect(data.failed24h).toBe(2);
    expect(data.audit).toEqual([{ ts: "2026-01-01T00:00:00Z", actor: "ana", action: "update_tenant", detail: {} }]);
  });
});
