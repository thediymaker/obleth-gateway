import { describe, expect, it, vi } from "vitest";
import { backfillMemberships, type BackfillDeps } from "./backfill-memberships";

const ID = { issuer: "https://idp", subject: "s1" };

function makeDeps(over: Partial<BackfillDeps> = {}) {
  const deps: BackfillDeps = {
    query: vi.fn(async () => ({ rows: [{ id: "u1", email: "a@x", tenantId: "t-shared" }] })),
    userIdentity: vi.fn(async () => ID),
    memberships: vi.fn(async () => []),
    upsertMember: vi.fn(async () => ({}) as never),
    ensurePersonalTenant: vi.fn(async () => "t-personal"),
    ...over,
  };
  return deps;
}

describe("backfillMemberships", () => {
  it("adds a member row for the legacy tenant and re-points tenantId", async () => {
    const deps = makeDeps();
    expect(await backfillMemberships(deps)).toBe(1);
    expect(deps.upsertMember).toHaveBeenCalledWith("t-shared", { ...ID, email: "a@x", role: "member" }, expect.anything());
    expect(deps.ensurePersonalTenant).toHaveBeenCalledWith("u1");
  });

  it("keeps an existing membership and its role", async () => {
    const deps = makeDeps({
      memberships: vi.fn(async () => [{ tenant_id: "t-shared", tenant_name: "s", kind: "standard" as const, role: "manager" as const }]),
    });
    await backfillMemberships(deps);
    expect(deps.upsertMember).not.toHaveBeenCalled();
    expect(deps.ensurePersonalTenant).toHaveBeenCalled();
  });

  it("leaves password users alone", async () => {
    const deps = makeDeps({ userIdentity: vi.fn(async () => null) });
    expect(await backfillMemberships(deps)).toBe(0);
    expect(deps.upsertMember).not.toHaveBeenCalled();
    expect(deps.ensurePersonalTenant).not.toHaveBeenCalled();
  });

  it("goes on to the next user when one fails", async () => {
    const deps = makeDeps({
      query: vi.fn(async () => ({ rows: [{ id: "u1", email: "a@x", tenantId: "t" }, { id: "u2", email: "b@x", tenantId: "t" }] })),
      upsertMember: vi.fn().mockRejectedValueOnce(new Error("down")).mockResolvedValue({}),
    });
    vi.spyOn(console, "error").mockImplementation(() => {});
    expect(await backfillMemberships(deps)).toBe(1);
  });
});
