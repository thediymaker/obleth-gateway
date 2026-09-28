import { afterEach, describe, expect, it, vi } from "vitest";
import type { NextRequest } from "next/server";
afterEach(() => vi.resetModules());

function mockSession(user: unknown) {
  vi.doMock("@/lib/auth/session", () => ({ getSession: async () => user }));
}

const params = Promise.resolve({ id: "m-1" });

describe("managed model live route", () => {
  it("attributes DELETE to the acting admin", async () => {
    mockSession({ id: "u", email: "admin@example.com", role: "admin", status: "active", tenantId: null });
    const deleteManagedModel = vi.fn().mockResolvedValue(undefined);
    vi.doMock("@/lib/obleth", () => ({ obleth: { deleteManagedModel } }));
    const { DELETE } = await import("./route");
    const res = await DELETE({} as NextRequest, { params });
    expect(res.status).toBe(200);
    expect(deleteManagedModel).toHaveBeenCalledWith("m-1", { auditActor: "admin@example.com" });
  });

  it("rejects a non-admin before touching the Management API", async () => {
    mockSession({ id: "u", email: "user@example.com", role: "user", status: "active", tenantId: "t" });
    const deleteManagedModel = vi.fn();
    vi.doMock("@/lib/obleth", () => ({ obleth: { deleteManagedModel } }));
    const { DELETE } = await import("./route");
    const res = await DELETE({} as NextRequest, { params });
    expect(res.status).toBe(401);
    expect(deleteManagedModel).not.toHaveBeenCalled();
  });
});
