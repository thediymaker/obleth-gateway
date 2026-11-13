import { afterEach, describe, expect, it, vi } from "vitest";
afterEach(() => vi.resetModules());

function setup(opts: { ids?: unknown[]; memberFails?: boolean } = {}) {
  vi.doMock("@/lib/auth/roles", () => ({
    requireAdmin: async () => ({ id: "a", email: "admin@example.com", role: "admin", status: "active", tenantId: null }),
  }));
  vi.doMock("next/cache", () => ({ revalidatePath: vi.fn(), updateTag: vi.fn() }));
  vi.doMock("@/lib/db", () => ({ getDb: () => ({ query: async () => ({ rows: [] }) }) }));
  const obleth = {
    createTenant: vi.fn().mockResolvedValue({ id: "t-1", name: "acme" }),
    deleteTenant: vi.fn().mockResolvedValue(undefined),
    updateTenant: vi.fn(),
    setTenantStatus: vi.fn(),
    setTenantSchedule: vi.fn(),
    setTenantBudget: vi.fn(),
    setTenantAllowlist: vi.fn(),
  };
  vi.doMock("@/lib/obleth", () => ({
    obleth,
    CACHE_TAGS: new Proxy({}, { get: () => "tag" }),
    OblethApiError: class OblethApiError extends Error {},
  }));
  const accounts = {
    identitiesByEmail: vi.fn().mockResolvedValue(opts.ids ?? [{ issuer: "iss", subject: "s1", email: "m@x.io", personal_tenant_id: null }]),
    upsertMember: opts.memberFails ? vi.fn().mockRejectedValue(new Error("member boom")) : vi.fn().mockResolvedValue({}),
  };
  vi.doMock("@/lib/obleth-accounts", () => ({ accounts }));
  return { obleth, accounts };
}

function form(fields: Record<string, string>): FormData {
  const fd = new FormData();
  for (const [k, v] of Object.entries(fields)) fd.set(k, v);
  return fd;
}

describe("createTenantAction manager email", () => {
  it("adds the manager as a member", async () => {
    const { accounts } = setup();
    const { createTenantAction } = await import("./actions");
    expect(await createTenantAction(form({ name: "acme", manager_email: "M@x.io" }))).toEqual({ ok: true });
    expect(accounts.upsertMember).toHaveBeenCalledWith("t-1", { issuer: "iss", subject: "s1", email: "m@x.io", role: "manager" }, { auditActor: "admin@example.com" });
  });

  it("no manager email adds no member", async () => {
    const { accounts } = setup();
    const { createTenantAction } = await import("./actions");
    expect(await createTenantAction(form({ name: "acme" }))).toEqual({ ok: true });
    expect(accounts.upsertMember).not.toHaveBeenCalled();
  });

  it("unknown manager creates nothing", async () => {
    const { obleth } = setup({ ids: [] });
    const { createTenantAction } = await import("./actions");
    expect(await createTenantAction(form({ name: "acme", manager_email: "m@x.io" }))).toEqual({ ok: false, error: "Manager m@x.io: User must sign in once first" });
    expect(obleth.createTenant).not.toHaveBeenCalled();
  });

  it("failed manager upsert deletes the tenant", async () => {
    const { obleth } = setup({ memberFails: true });
    const { createTenantAction } = await import("./actions");
    const r = await createTenantAction(form({ name: "acme", manager_email: "m@x.io" }));
    expect(r.ok).toBe(false);
    expect(obleth.deleteTenant).toHaveBeenCalledWith("t-1", { auditActor: "admin@example.com" });
  });
});
