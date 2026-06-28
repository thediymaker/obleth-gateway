import { afterEach, describe, expect, it, vi } from "vitest";
afterEach(() => vi.resetModules());

// actions.ts imports from:
//   "next/cache"     -> revalidatePath, updateTag
//   "@/lib/obleth"   -> CACHE_TAGS, obleth, OblethApiError
//   "@/lib/auth/roles" -> requireAdmin
//   "@/lib/sbatch-recipes" -> resolveRecipeById, buildManagedFromRecipe, parseRecipe

function mockAdmin() {
  vi.doMock("@/lib/auth/roles", () => ({
    requireAdmin: async () => ({
      id: "admin-1",
      email: "admin@example.com",
      role: "admin",
      status: "active",
      tenantId: null,
    }),
  }));
  vi.doMock("next/cache", () => ({
    revalidatePath: vi.fn(),
    updateTag: vi.fn(),
  }));
}

describe("admin actions attribute the acting user", () => {
  it("createTenantAction passes auditActor", async () => {
    mockAdmin();
    const createTenant = vi
      .fn()
      .mockResolvedValue({ id: "t-1", name: "Acme" });
    vi.doMock("@/lib/obleth", () => ({
      obleth: {
        createTenant,
        updateTenant: vi.fn().mockResolvedValue({}),
        setTenantStatus: vi.fn().mockResolvedValue({}),
        setTenantSchedule: vi.fn().mockResolvedValue({}),
        setTenantBudget: vi.fn().mockResolvedValue({}),
        setTenantAllowlist: vi.fn().mockResolvedValue({}),
      },
      CACHE_TAGS: new Proxy({}, { get: () => "tag" }),
      OblethApiError: class OblethApiError extends Error {},
    }));
    const { createTenantAction } = await import("./actions");
    const fd = new FormData();
    fd.set("name", "Acme");
    await createTenantAction(fd);
    expect(createTenant).toHaveBeenCalledWith(
      expect.anything(),
      expect.objectContaining({ auditActor: "admin@example.com" }),
    );
  });

  it("deleteModelAction passes auditActor", async () => {
    mockAdmin();
    const deleteModel = vi.fn().mockResolvedValue(undefined);
    vi.doMock("@/lib/obleth", () => ({
      obleth: { deleteModel },
      CACHE_TAGS: new Proxy({}, { get: () => "tag" }),
      OblethApiError: class OblethApiError extends Error {},
    }));
    const { deleteModelAction } = await import("./actions");
    await deleteModelAction("m-1");
    expect(deleteModel).toHaveBeenCalledWith("m-1", {
      auditActor: "admin@example.com",
    });
  });

  it("setBoonSettingsAction passes auditActor", async () => {
    mockAdmin();
    const setBoonSettings = vi.fn().mockResolvedValue(undefined);
    vi.doMock("@/lib/obleth", () => ({
      obleth: { setBoonSettings },
      CACHE_TAGS: new Proxy({}, { get: () => "tag" }),
      OblethApiError: class OblethApiError extends Error {},
    }));
    const { setBoonSettingsAction } = await import("./actions");
    // setBoonSettingsAction takes UpdateBoonSettings body, not FormData
    await setBoonSettingsAction({} as any);
    expect(setBoonSettings).toHaveBeenCalledWith(
      expect.anything(),
      expect.objectContaining({ auditActor: "admin@example.com" }),
    );
  });
});
