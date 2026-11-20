import { afterEach, describe, expect, it, vi } from "vitest";
afterEach(() => vi.resetModules());

const ADMIN_ID = "admin-1";
const OTHER_ID = "admin-2";

// The last-admin decision itself is made atomically in lib/auth/users.ts (see
// users.test.ts); here `result` is what that layer reports back.
function setup(result: "ok" | "not-found" | "last-admin" = "ok") {
  const assignUser = vi.fn().mockResolvedValue(result);
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
  return { assignUser, setUserStatus, revalidatePath };
}

describe("setUserStatusAction", () => {
  it("refuses to deactivate the caller's own account", async () => {
    const { setUserStatus } = setup();
    const { setUserStatusAction } = await import("./users-actions");
    const res = await setUserStatusAction(ADMIN_ID, "pending");
    expect(res).toEqual({ ok: false, error: "You cannot remove admin access from your own account" });
    expect(setUserStatus).not.toHaveBeenCalled();
  });

  it("reports the last-admin refusal and does not revalidate", async () => {
    const { setUserStatus, revalidatePath } = setup("last-admin");
    const { setUserStatusAction } = await import("./users-actions");
    const res = await setUserStatusAction(OTHER_ID, "pending");
    expect(res).toEqual({ ok: false, error: "Cannot remove the last active admin" });
    expect(setUserStatus).toHaveBeenCalledWith(OTHER_ID, "pending");
    expect(revalidatePath).not.toHaveBeenCalled();
  });

  it("deactivates another user", async () => {
    const { setUserStatus, revalidatePath } = setup("ok");
    const { setUserStatusAction } = await import("./users-actions");
    const res = await setUserStatusAction(OTHER_ID, "pending");
    expect(res.ok).toBe(true);
    expect(setUserStatus).toHaveBeenCalledWith(OTHER_ID, "pending");
    expect(revalidatePath).toHaveBeenCalledWith("/users");
  });
});

describe("assignUserAction", () => {
  it("refuses to demote the caller's own account", async () => {
    const { assignUser } = setup();
    const { assignUserAction } = await import("./users-actions");
    const res = await assignUserAction({ id: ADMIN_ID, role: "user", tenantId: "" });
    expect(res.ok).toBe(false);
    expect(assignUser).not.toHaveBeenCalled();
  });

  it("reports the last-admin refusal", async () => {
    setup("last-admin");
    const { assignUserAction } = await import("./users-actions");
    const res = await assignUserAction({ id: OTHER_ID, role: "user", tenantId: "" });
    expect(res).toEqual({ ok: false, error: "Cannot remove the last active admin" });
  });

  it("lets an admin reassign their own tenant while staying admin", async () => {
    const { assignUser } = setup("ok");
    const { assignUserAction } = await import("./users-actions");
    const res = await assignUserAction({ id: ADMIN_ID, role: "admin", tenantId: "" });
    expect(res.ok).toBe(true);
    expect(assignUser).toHaveBeenCalledWith(ADMIN_ID, "admin", null);
  });

  it("reports an unknown user", async () => {
    setup("not-found");
    const { assignUserAction } = await import("./users-actions");
    const res = await assignUserAction({ id: OTHER_ID, role: "user", tenantId: "" });
    expect(res).toEqual({ ok: false, error: "User not found" });
  });
});
