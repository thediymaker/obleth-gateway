import { afterEach, describe, expect, it, vi } from "vitest";
afterEach(() => vi.resetModules());

const ADMIN_ID = "admin-1";
const OTHER_ID = "admin-2";

// The last-admin decision itself is made atomically in lib/auth/users.ts (see
// users.test.ts); here `result` is what that layer reports back.
const T1 = "11111111-1111-1111-1111-111111111111";
const T2 = "22222222-2222-2222-2222-222222222222";
const ID = { issuer: "iss", subject: "s1" };

function setup(result: "ok" | "not-found" | "last-admin" = "ok", identity: typeof ID | null = ID) {
  const assignUser = vi.fn().mockResolvedValue(result);
  const accounts = {
    memberships: vi.fn().mockResolvedValue([
      { tenant_id: T1, tenant_name: "a", kind: "standard", role: "manager" },
      { tenant_id: "p", tenant_name: "me", kind: "personal", role: "owner" },
    ]),
    upsertMember: vi.fn().mockResolvedValue({}),
    removeMember: vi.fn().mockResolvedValue(undefined),
  };
  vi.doMock("@/lib/auth/personal", () => ({ userIdentity: vi.fn(async () => identity) }));
  vi.doMock("@/lib/db", () => ({ getDb: () => ({ query: async () => ({ rows: [{ email: "u@x.io" }] }) }) }));
  vi.doMock("@/lib/obleth-accounts", () => ({ accounts }));
  const setUserStatus = vi.fn().mockResolvedValue(result);
  const revalidatePath = vi.fn();
  vi.doMock("@/lib/auth/roles", () => ({
    requireAdmin: async () => ({
      id: ADMIN_ID,
      email: "admin@example.com",
      role: "admin",
      status: "active",
      tenantId: null,
    }),
  }));
  vi.doMock("@/lib/auth/users", () => ({ assignUser, setUserStatus }));
  vi.doMock("next/cache", () => ({ revalidatePath }));
  return { assignUser, setUserStatus, revalidatePath, accounts };
}

function form(fields: Record<string, string | string[]>): FormData {
  const fd = new FormData();
  for (const [k, v] of Object.entries(fields)) for (const x of [v].flat()) fd.append(k, x);
  return fd;
}

describe("setUserStatusAction", () => {
  it("refuses to deactivate the caller's own account", async () => {
    const { setUserStatus } = setup();
    const { setUserStatusAction } = await import("./users-actions");
    const res = await setUserStatusAction(form({ id: ADMIN_ID, status: "pending" }));
    expect(res).toEqual({ ok: false, error: "You cannot remove admin access from your own account" });
    expect(setUserStatus).not.toHaveBeenCalled();
  });

  it("reports the last-admin refusal and does not revalidate", async () => {
    const { setUserStatus, revalidatePath } = setup("last-admin");
    const { setUserStatusAction } = await import("./users-actions");
    const res = await setUserStatusAction(form({ id: OTHER_ID, status: "pending" }));
    expect(res).toEqual({ ok: false, error: "Cannot remove the last active admin" });
    expect(setUserStatus).toHaveBeenCalledWith(OTHER_ID, "pending");
    expect(revalidatePath).not.toHaveBeenCalled();
  });

  it("deactivates another user", async () => {
    const { setUserStatus, revalidatePath } = setup("ok");
    const { setUserStatusAction } = await import("./users-actions");
    const res = await setUserStatusAction(form({ id: OTHER_ID, status: "pending" }));
    expect(res.ok).toBe(true);
    expect(setUserStatus).toHaveBeenCalledWith(OTHER_ID, "pending");
    expect(revalidatePath).toHaveBeenCalledWith("/users");
  });
});

describe("assignUserAction", () => {
  it("refuses to demote the caller's own account", async () => {
    const { assignUser } = setup();
    const { assignUserAction } = await import("./users-actions");
    const res = await assignUserAction(form({ id: ADMIN_ID, role: "user" }));
    expect(res.ok).toBe(false);
    expect(assignUser).not.toHaveBeenCalled();
  });

  it("reports the last-admin refusal", async () => {
    setup("last-admin");
    const { assignUserAction } = await import("./users-actions");
    const res = await assignUserAction(form({ id: OTHER_ID, role: "user" }));
    expect(res).toEqual({ ok: false, error: "Cannot remove the last active admin" });
  });

  it("lets an admin change their own tenants while staying admin", async () => {
    const { assignUser } = setup("ok");
    const { assignUserAction } = await import("./users-actions");
    const res = await assignUserAction(form({ id: ADMIN_ID, role: "admin" }));
    expect(res.ok).toBe(true);
    expect(assignUser).toHaveBeenCalledWith(ADMIN_ID, "admin");
  });

  it("reports an unknown user", async () => {
    setup("not-found");
    const { assignUserAction } = await import("./users-actions");
    const res = await assignUserAction(form({ id: OTHER_ID, role: "user" }));
    expect(res).toEqual({ ok: false, error: "User not found" });
  });

  it("syncs tenants: adds new as member, keeps existing, removes unselected, never personal", async () => {
    const { accounts } = setup();
    const { assignUserAction } = await import("./users-actions");
    expect(await assignUserAction(form({ id: OTHER_ID, role: "user", syncTenants: "1", tenantIds: [T2], knownTenantIds: [T1] }))).toEqual({ ok: true });
    expect(accounts.upsertMember).toHaveBeenCalledTimes(1);
    expect(accounts.upsertMember).toHaveBeenCalledWith(T2, { ...ID, email: "u@x.io", role: "member" }, { auditActor: "admin@example.com" });
    expect(accounts.removeMember).toHaveBeenCalledTimes(1);
    expect(accounts.removeMember).toHaveBeenCalledWith(T1, "iss", "s1", { auditActor: "admin@example.com" });
  });

  it("keeps a membership the admin didn't see (added since the page loaded)", async () => {
    const { accounts } = setup();
    const { assignUserAction } = await import("./users-actions");
    expect(await assignUserAction(form({ id: OTHER_ID, role: "user", syncTenants: "1", tenantIds: [T2] }))).toEqual({ ok: true });
    expect(accounts.upsertMember).toHaveBeenCalledTimes(1);
    expect(accounts.removeMember).not.toHaveBeenCalled();
  });

  it("keeps a selected membership as is", async () => {
    const { accounts } = setup();
    const { assignUserAction } = await import("./users-actions");
    await assignUserAction(form({ id: OTHER_ID, role: "user", syncTenants: "1", tenantIds: [T1] }));
    expect(accounts.upsertMember).not.toHaveBeenCalled();
    expect(accounts.removeMember).not.toHaveBeenCalled();
  });

  it("without syncTenants leaves memberships alone", async () => {
    const { accounts } = setup();
    const { assignUserAction } = await import("./users-actions");
    await assignUserAction(form({ id: OTHER_ID, role: "user" }));
    expect(accounts.memberships).not.toHaveBeenCalled();
  });

  it("refuses tenants for a password account", async () => {
    setup("ok", null);
    const { assignUserAction } = await import("./users-actions");
    const res = await assignUserAction(form({ id: OTHER_ID, role: "user", syncTenants: "1", tenantIds: [T1] }));
    expect(res).toEqual({ ok: false, error: "Role saved, tenants not: a password account can't be a tenant member" });
  });
});

