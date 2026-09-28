import { afterEach, describe, expect, it, vi } from "vitest";

afterEach(() => vi.resetModules());

const report = {
  keys: 12,
  keys_pruned: 1,
  models: 3,
  model_names_pruned: 0,
  mcp_servers: 2,
  mcp_servers_pruned: 0,
};

function mockDeps(resync: ReturnType<typeof vi.fn>, requireAdmin = vi.fn()) {
  requireAdmin.mockResolvedValue({
    id: "admin-1",
    email: "admin@example.com",
    role: "admin",
    status: "active",
    tenantId: null,
  });
  vi.doMock("@/lib/auth/roles", () => ({ requireAdmin }));
  vi.doMock("next/cache", () => ({ revalidatePath: vi.fn(), updateTag: vi.fn() }));
  vi.doMock("@/lib/obleth", () => ({
    obleth: { resync },
    CACHE_TAGS: new Proxy({}, { get: () => "tag" }),
    OblethApiError: class OblethApiError extends Error {},
  }));
  return requireAdmin;
}

describe("resyncCacheAction", () => {
  it("requires an admin and attributes the reconcile to the acting user", async () => {
    const resync = vi.fn().mockResolvedValue(report);
    const requireAdmin = mockDeps(resync);
    const { resyncCacheAction } = await import("./actions");

    const result = await resyncCacheAction();

    expect(requireAdmin).toHaveBeenCalledTimes(1);
    expect(resync).toHaveBeenCalledWith({ auditActor: "admin@example.com" });
    expect(result).toEqual({ ok: true, report });
  });

  it("does not call the gateway when the caller is not an admin", async () => {
    const resync = vi.fn();
    const requireAdmin = vi.fn();
    mockDeps(resync, requireAdmin);
    requireAdmin.mockReset().mockRejectedValue(new Error("NEXT_REDIRECT"));
    const { resyncCacheAction } = await import("./actions");

    await expect(resyncCacheAction()).rejects.toThrow("NEXT_REDIRECT");
    expect(resync).not.toHaveBeenCalled();
  });

  it("returns the gateway's error message when the reconcile fails", async () => {
    const resync = vi.fn().mockRejectedValue(new Error("cache sync failed"));
    mockDeps(resync);
    const { resyncCacheAction } = await import("./actions");

    await expect(resyncCacheAction()).resolves.toEqual({ ok: false, error: "cache sync failed" });
  });
});
