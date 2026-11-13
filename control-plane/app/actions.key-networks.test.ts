import { afterEach, describe, expect, it, vi } from "vitest";
afterEach(() => vi.resetModules());

// Same mocking approach as actions.key-background.test.ts.
function mockObleth(extra: Record<string, unknown> = {}) {
  vi.doMock("@/lib/auth/roles", () => ({
    requireAdmin: async () => ({
      id: "admin-1",
      email: "admin@example.com",
      role: "admin",
      status: "active",
      tenantId: null,
    }),
  }));
  vi.doMock("next/cache", () => ({ revalidatePath: vi.fn(), updateTag: vi.fn() }));
  const obleth = {
    createKey: vi.fn().mockResolvedValue({ secret: "obl_new", key: { id: "k2" } }),
    updateKey: vi.fn().mockResolvedValue({}),
    ...extra,
  };
  vi.doMock("@/lib/obleth", () => ({
    obleth,
    CACHE_TAGS: new Proxy({}, { get: () => "tag" }),
    OblethApiError: class OblethApiError extends Error {},
  }));
  return obleth;
}

const oldKey = {
  id: "k1", tenant_id: "t1", name: "lab", description: "", key_prefix: "obl_abc", kind: "secret",
  identity_issuer: null, identity_subject: null, identity_claims: null, weight: 100, max_in_flight: null,
  budget_tokens: null, budget_cost_usd: null, budget_period: null, budget_started_at: null,
  disabled: false, tracing_enabled: false, end_user_fairshare: false, background: false,
  allowed_networks: ["203.0.113.0/24"], created_at: "", updated_at: "",
};

describe("allowed networks on a key", () => {
  it("are sent one per line when a key is made", async () => {
    const obleth = mockObleth();
    const { createKeyAction } = await import("./actions");
    const fd = new FormData();
    fd.set("tenant_id", "t1");
    fd.set("name", "lab");
    fd.set("allowed_networks", "203.0.113.0/24\n\n 2001:db8::/48 \n");
    expect((await createKeyAction(fd)).ok).toBe(true);
    expect(obleth.createKey.mock.calls[0][1].allowed_networks).toEqual(["203.0.113.0/24", "2001:db8::/48"]);
  });

  it("are saved with the key's fields, and an empty box allows any network", async () => {
    const obleth = mockObleth();
    const { saveKeySettingsAction } = await import("./actions");
    const fd = new FormData();
    fd.set("id", "k1");
    fd.set("sections", "key");
    fd.set("name", "lab");
    fd.set("allowed_networks", "");
    expect(await saveKeySettingsAction(fd)).toEqual({ ok: true });
    expect(obleth.updateKey.mock.calls[0][1].allowed_networks).toEqual([]);
  });

  it("are left alone by an edit that doesn't show them", async () => {
    const obleth = mockObleth({ listKeys: vi.fn().mockResolvedValue([oldKey]) });
    const { setKeysBudgetAction, updateKeyAction } = await import("./actions");
    const fd = new FormData();
    fd.set("id", "k1");
    fd.set("name", "lab");
    await updateKeyAction(fd);
    await setKeysBudgetAction(["k1"], { budget_tokens: 100, budget_cost_usd: null, budget_period: "monthly" });
    for (const call of obleth.updateKey.mock.calls) expect(call[1]).not.toHaveProperty("allowed_networks");
  });

  it("are kept when the key is replaced", async () => {
    const obleth = mockObleth({ listKeys: vi.fn().mockResolvedValue([oldKey]) });
    const { replaceKeyAction } = await import("./actions");
    expect((await replaceKeyAction("k1")).ok).toBe(true);
    expect(obleth.createKey.mock.calls[0][1].allowed_networks).toEqual(["203.0.113.0/24"]);
  });
});
