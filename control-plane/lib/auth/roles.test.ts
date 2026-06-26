import { afterEach, describe, expect, it, vi } from "vitest";
afterEach(() => vi.resetModules());

function mockSession(user: unknown) {
  vi.doMock("@/lib/auth/session", () => ({ getSession: async () => user }));
}

describe("role guards", () => {
  it("requireAdmin rejects a non-admin", async () => {
    mockSession({ id: "u", email: "x", role: "user", status: "active", tenantId: "t" });
    const { requireAdmin } = await import("./roles");
    await expect(requireAdmin()).rejects.toThrow(/Unauthorized/);
  });
  it("requireAdmin allows an active admin", async () => {
    mockSession({ id: "u", email: "x", role: "admin", status: "active", tenantId: null });
    const { requireAdmin } = await import("./roles");
    await expect(requireAdmin()).resolves.toMatchObject({ role: "admin" });
  });
  it("requireTenant throws when tenantId is null", async () => {
    mockSession({ id: "u", email: "x", role: "user", status: "active", tenantId: null });
    const { requireTenant } = await import("./roles");
    await expect(requireTenant()).rejects.toThrow(/tenant/i);
  });
  it("requireUser rejects a pending user", async () => {
    mockSession({ id: "u", email: "pending@example.com", role: "user", status: "pending", tenantId: null });
    const { requireUser } = await import("./roles");
    await expect(requireUser()).rejects.toThrow(/Unauthorized/);
  });
});
