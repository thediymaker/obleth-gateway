import { afterEach, describe, expect, it, vi } from "vitest";
afterEach(() => vi.resetModules());

const TID = "7b0f5c1e-1d2a-4c3b-8e4f-5a6b7c8d9e0f";

function setup(opts: { admin?: boolean; ids?: unknown[] } = {}) {
  const requireAdmin = vi.fn(async () => {
    if (opts.admin === false) throw new Error("Unauthorized");
    return { id: "a", email: "admin@example.com", role: "admin", status: "active", tenantId: null };
  });
  const accounts = {
    identitiesByEmail: vi.fn().mockResolvedValue(opts.ids ?? [{ issuer: "iss", subject: "s1", email: "m@x", personal_tenant_id: null }]),
    upsertMember: vi.fn().mockResolvedValue({}),
    removeMember: vi.fn().mockResolvedValue(undefined),
    listMembers: vi.fn().mockResolvedValue([{ issuer: "iss", subject: "s1", email: "m@x", role: "member" }]),
  };
  vi.doMock("@/lib/auth/roles", () => ({ requireAdmin }));
  vi.doMock("@/lib/obleth-accounts", () => ({ accounts }));
  vi.doMock("@/lib/db", () => ({ getDb: () => ({ query: async () => ({ rows: [] }) }) }));
  vi.doMock("next/cache", () => ({ revalidatePath: vi.fn() }));
  return { accounts };
}

function form(fields: Record<string, string>): FormData {
  const fd = new FormData();
  for (const [k, v] of Object.entries(fields)) fd.set(k, v);
  return fd;
}

describe("tenant members actions", () => {
  it("require admin", async () => {
    const { accounts } = setup({ admin: false });
    const m = await import("./members-actions");
    await expect(m.addTenantMember(form({}))).rejects.toThrow("Unauthorized");
    await expect(m.removeTenantMember(form({}))).rejects.toThrow("Unauthorized");
    expect(accounts.upsertMember).not.toHaveBeenCalled();
    expect(accounts.removeMember).not.toHaveBeenCalled();
  });

  it("addTenantMember by email; unknown email refused", async () => {
    const { accounts } = setup();
    const m = await import("./members-actions");
    expect(await m.addTenantMember(form({ tenantId: TID, email: "B@x.io", role: "manager" }))).toEqual({ ok: true });
    expect(accounts.upsertMember).toHaveBeenCalledWith(TID, { issuer: "iss", subject: "s1", email: "b@x.io", role: "manager" }, { auditActor: "admin@example.com" });
    accounts.identitiesByEmail.mockResolvedValue([]);
    expect(await m.addTenantMember(form({ tenantId: TID, email: "c@x.io", role: "member" }))).toEqual({ ok: false, error: "User must sign in once first" });
  });

  it("multiple identities for one email adds nothing", async () => {
    const { accounts } = setup({ ids: [
      { issuer: "i1", subject: "s1", email: "m@x", personal_tenant_id: null },
      { issuer: "i2", subject: "s2", email: "m@x", personal_tenant_id: null },
    ] });
    const m = await import("./members-actions");
    expect(await m.addTenantMember(form({ tenantId: TID, email: "m@x.io", role: "member" }))).toEqual({ ok: false, error: "Email matches multiple identities" });
    expect(accounts.upsertMember).not.toHaveBeenCalled();
  });

  it("removeTenantMember", async () => {
    const { accounts } = setup();
    const { removeTenantMember } = await import("./members-actions");
    expect(await removeTenantMember(form({ tenantId: TID, issuer: "iss", subject: "s1" }))).toEqual({ ok: true });
    expect(accounts.removeMember).toHaveBeenCalledWith(TID, "iss", "s1", { auditActor: "admin@example.com" });
  });

  it("removeTenantMember refuses the owner", async () => {
    const { accounts } = setup();
    accounts.listMembers.mockResolvedValue([{ issuer: "iss", subject: "s1", email: "m@x", role: "owner" }]);
    const { removeTenantMember } = await import("./members-actions");
    expect(await removeTenantMember(form({ tenantId: TID, issuer: "iss", subject: "s1" }))).toEqual({ ok: false, error: "The owner can't be removed" });
    expect(accounts.removeMember).not.toHaveBeenCalled();
  });

  it("gateway error comes back as a result", async () => {
    const { accounts } = setup();
    accounts.upsertMember.mockRejectedValue(new Error("personal tenants have no members"));
    const { addTenantMember } = await import("./members-actions");
    expect(await addTenantMember(form({ tenantId: TID, email: "b@x.io", role: "member" }))).toEqual({ ok: false, error: "personal tenants have no members" });
  });
});
