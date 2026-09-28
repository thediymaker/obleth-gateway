import { afterEach, describe, expect, it, vi } from "vitest";
afterEach(() => vi.resetModules());

function mockSession(user: unknown) {
  vi.doMock("@/lib/auth/session", () => ({ getSession: async () => user }));
}

describe("guardAdmin", () => {
  it("returns null (proceed) for an active admin", async () => {
    mockSession({ id: "u", email: "a", role: "admin", status: "active", tenantId: null });
    const { guardAdmin } = await import("./guard");
    expect(await guardAdmin()).toBeNull();
  });

  it("returns 401 for an active non-admin (portal user)", async () => {
    mockSession({ id: "u", email: "u", role: "user", status: "active", tenantId: "t" });
    const { guardAdmin } = await import("./guard");
    const res = await guardAdmin();
    expect(res).not.toBeNull();
    expect(res?.status).toBe(401);
  });

  it("returns 401 when there is no session at all", async () => {
    mockSession(null);
    const { guardAdmin } = await import("./guard");
    const res = await guardAdmin();
    expect(res?.status).toBe(401);
  });

  it("returns 401 for a pending admin (not yet active)", async () => {
    mockSession({ id: "u", email: "a", role: "admin", status: "pending", tenantId: null });
    const { guardAdmin } = await import("./guard");
    const res = await guardAdmin();
    expect(res?.status).toBe(401);
  });
});

describe("guardAdminSession", () => {
  it("returns the admin session alongside a null denial", async () => {
    const admin = { id: "u", email: "a@example.com", role: "admin", status: "active", tenantId: null };
    mockSession(admin);
    const { guardAdminSession } = await import("./guard");
    const { denied, session } = await guardAdminSession();
    expect(denied).toBeNull();
    expect(session).toEqual(admin);
  });

  it("returns a 401 and no session for a non-admin", async () => {
    mockSession({ id: "u", email: "u", role: "user", status: "active", tenantId: "t" });
    const { guardAdminSession } = await import("./guard");
    const { denied, session } = await guardAdminSession();
    expect(denied?.status).toBe(401);
    expect(session).toBeNull();
  });
});
