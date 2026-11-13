import { afterEach, describe, expect, it, vi } from "vitest";
afterEach(() => vi.resetModules());

const T = "tenant-1";
type Role = "owner" | "manager" | "member";

function setup(o: { role?: Role; kind?: string; ids?: unknown[]; members?: unknown[] } = {}) {
  const ctx = {
    identity: { issuer: "iss", subject: "me" },
    tenants: [],
    current: { id: T, name: "p-alpha", kind: o.kind ?? "standard", role: o.role ?? "manager" },
  };
  const accounts = {
    identitiesByEmail: vi.fn().mockResolvedValue(o.ids ?? [{ issuer: "iss", subject: "b", email: "b@x", personal_tenant_id: null }]),
    listMembers: vi.fn().mockResolvedValue(
      o.members ?? [
        { issuer: "iss", subject: "me", role: "manager" },
        { issuer: "iss", subject: "m2", role: "manager" },
      ],
    ),
    upsertMember: vi.fn().mockResolvedValue({}),
    removeMember: vi.fn().mockResolvedValue(undefined),
  };
  vi.doMock("@/lib/auth/roles", () => ({ requireUser: async () => ({ id: "u", email: "me@x", status: "active", role: "user", tenantId: null }) }));
  vi.doMock("@/lib/portal-context", () => ({ currentPortalContext: async () => ctx }));
  vi.doMock("@/lib/obleth-accounts", () => ({ accounts }));
  vi.doMock("@/lib/db", () => ({ getDb: () => ({ query: async () => ({ rows: [] }) }) }));
  vi.doMock("next/cache", () => ({ revalidatePath: vi.fn() }));
  return accounts;
}

function form(f: Record<string, string>) {
  const fd = new FormData();
  for (const [k, v] of Object.entries(f)) fd.set(k, v);
  return fd;
}

const REFUSED = { ok: false, error: "Only a tenant manager can manage members" };

describe("members actions", () => {
  it("manager adds lowercased email on the context tenant, ignoring form tenantId", async () => {
    const a = setup();
    const { addMember } = await import("./members-actions");
    expect(await addMember(form({ email: "B@x.io", role: "member", tenantId: "evil" }))).toEqual({ ok: true });
    expect(a.upsertMember).toHaveBeenCalledWith(T, { issuer: "iss", subject: "b", email: "b@x.io", role: "member" }, { auditActor: "me@x" });
    expect(a.identitiesByEmail).toHaveBeenCalledWith("b@x.io");
  });

  it("non_manager_refused for add/remove/role", async () => {
    const a = setup({ role: "member" });
    const m = await import("./members-actions");
    expect(await m.addMember(form({ email: "b@x.io", role: "member" }))).toEqual(REFUSED);
    expect(await m.addMember(form({ email: "b@x.io", role: "manager" }))).toEqual(REFUSED);
    expect(await m.removeMember(form({ issuer: "iss", subject: "b" }))).toEqual(REFUSED);
    expect(a.upsertMember).not.toHaveBeenCalled();
    expect(a.removeMember).not.toHaveBeenCalled();
  });

  it("personal tenant refused", async () => {
    const a = setup({ kind: "personal", role: "owner" });
    const m = await import("./members-actions");
    expect(await m.addMember(form({ email: "b@x.io", role: "member" }))).toEqual(REFUSED);
    expect(a.upsertMember).not.toHaveBeenCalled();
  });

  it("unknown_email", async () => {
    setup({ ids: [] });
    const { addMember } = await import("./members-actions");
    expect(await addMember(form({ email: "z@x.io", role: "member" }))).toEqual({ ok: false, error: "User must sign in once first" });
  });

  it("cannot_remove_self_as_last_manager", async () => {
    const a = setup({ members: [{ issuer: "iss", subject: "me", role: "manager" }, { issuer: "iss", subject: "b", role: "member" }] });
    const { removeMember } = await import("./members-actions");
    expect(await removeMember(form({ issuer: "iss", subject: "me" }))).toEqual({ ok: false, error: "A tenant needs a manager" });
    expect(a.removeMember).not.toHaveBeenCalled();
  });

  it("demoting the last manager refused", async () => {
    const a = setup({
      ids: [{ issuer: "iss", subject: "me", email: "me@x.io", personal_tenant_id: null }],
      members: [{ issuer: "iss", subject: "me", role: "manager" }],
    });
    const { addMember } = await import("./members-actions");
    expect(await addMember(form({ email: "me@x.io", role: "member" }))).toEqual({ ok: false, error: "A tenant needs a manager" });
    expect(a.upsertMember).not.toHaveBeenCalled();
  });

  it("removes a non-last manager", async () => {
    const a = setup();
    const { removeMember } = await import("./members-actions");
    expect(await removeMember(form({ issuer: "iss", subject: "m2" }))).toEqual({ ok: true });
    expect(a.removeMember).toHaveBeenCalledWith(T, "iss", "m2", { auditActor: "me@x" });
  });

  it("owner cannot be demoted or removed from the portal", async () => {
    const a = setup({
      ids: [{ issuer: "iss", subject: "own", email: "o@x", personal_tenant_id: null }],
      members: [
        { issuer: "iss", subject: "me", role: "manager" },
        { issuer: "iss", subject: "own", role: "owner" },
      ],
    });
    const m = await import("./members-actions");
    expect((await m.addMember(form({ email: "o@x", role: "member" }))).ok).toBe(false);
    expect((await m.removeMember(form({ issuer: "iss", subject: "own" }))).ok).toBe(false);
    expect(a.upsertMember).not.toHaveBeenCalled();
    expect(a.removeMember).not.toHaveBeenCalled();
  });

  it("multiple identities for one email adds nobody", async () => {
    const a = setup({
      ids: [
        { issuer: "i1", subject: "b", email: "b@x", personal_tenant_id: null },
        { issuer: "i2", subject: "c", email: "b@x", personal_tenant_id: null },
      ],
    });
    const { addMember } = await import("./members-actions");
    expect(await addMember(form({ email: "b@x.io", role: "member" }))).toEqual({ ok: false, error: "Email matches multiple identities" });
    expect(a.upsertMember).not.toHaveBeenCalled();
  });
});
