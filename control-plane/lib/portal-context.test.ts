import { afterEach, describe, expect, it, vi } from "vitest";

afterEach(() => vi.resetModules());

const alice = { id: "u", email: "a@x", role: "user", status: "active", tenantId: null } as const;
const memberships = [
  { tenant_id: "p1", tenant_name: "p-alpha", kind: "standard", role: "member" },
  { tenant_id: "u1", tenant_name: "u-s1", kind: "personal", role: "owner" },
];

async function load(identity: unknown) {
  vi.doMock("next/headers", () => ({ cookies: async () => ({ get: () => undefined }) }));
  vi.doMock("@/lib/auth/session", () => ({ getSession: async () => alice }));
  vi.doMock("@/lib/auth/personal", () => ({ userIdentity: async () => identity }));
  vi.doMock("@/lib/obleth-accounts", () => ({ accounts: { memberships: async () => memberships } }));
  return import("./portal-context");
}

describe("loadPortalContext", () => {
  const id = { issuer: "iss", subject: "s1" };
  it("selects a requested tenant the user belongs to", async () => {
    const { loadPortalContext } = await load(id);
    const ctx = await loadPortalContext(alice, "p1");
    expect(ctx.identity).toEqual(id);
    expect(ctx.tenants).toContainEqual({ id: "u1", name: "u-s1", kind: "personal", role: "owner" });
    expect(ctx.current.id).toBe("p1");
  });
  it("falls back to the personal tenant for a foreign id", async () => {
    const { loadPortalContext } = await load(id);
    expect((await loadPortalContext(alice, "p-not-mine")).current.id).toBe("u1");
  });
  it("throws without identity", async () => {
    const { loadPortalContext } = await load(null);
    await expect(loadPortalContext(alice)).rejects.toThrow("No identity");
  });
});
