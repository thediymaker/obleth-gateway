import { describe, expect, it } from "vitest";
import { awaitingApprovalTarget } from "./page";

describe("awaitingApprovalTarget", () => {
  it("returns /login when there is no session", () => {
    expect(awaitingApprovalTarget(null)).toBe("/login");
  });

  it("returns / for an active admin (regardless of tenantId)", () => {
    expect(
      awaitingApprovalTarget({ id: "u1", email: "a@x.com", role: "admin", status: "active", tenantId: null }),
    ).toBe("/");
    expect(
      awaitingApprovalTarget({ id: "u1", email: "a@x.com", role: "admin", status: "active", tenantId: "t1" }),
    ).toBe("/");
  });

  it("returns /portal/models for an active user WITH a tenant", () => {
    expect(
      awaitingApprovalTarget({ id: "u2", email: "b@x.com", role: "user", status: "active", tenantId: "t1" }),
    ).toBe("/portal/models");
  });

  it("returns null (no redirect) for an active user WITHOUT a tenant — no infinite loop", () => {
    // This is the loop-breaking case: active+user+no-tenant must NOT redirect to /portal/models
    const target = awaitingApprovalTarget({
      id: "u3",
      email: "c@x.com",
      role: "user",
      status: "active",
      tenantId: null,
    });
    expect(target).toBeNull();
    expect(target).not.toBe("/portal/models");
  });

  it("returns null for a pending user (stay on page)", () => {
    expect(
      awaitingApprovalTarget({ id: "u4", email: "d@x.com", role: "user", status: "pending", tenantId: null }),
    ).toBeNull();
    expect(
      awaitingApprovalTarget({ id: "u4", email: "d@x.com", role: "user", status: "pending", tenantId: "t1" }),
    ).toBeNull();
  });
});
