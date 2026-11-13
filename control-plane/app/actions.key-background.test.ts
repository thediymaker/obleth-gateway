import { afterEach, describe, expect, it, vi } from "vitest";
afterEach(() => vi.resetModules());

// Same mocking approach as actions.auto-eligible.test.ts: actions.ts pulls in
// "next/cache", "@/lib/obleth" and "@/lib/auth/roles", none of which can run
// under vitest.
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
    setKeyBackground: vi.fn().mockResolvedValue(undefined),
    setKeyEndUserFairshare: vi.fn().mockResolvedValue(undefined),
    setKeyTracing: vi.fn().mockResolvedValue(undefined),
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
  id: "k1", tenant_id: "t1", name: "runner", description: "", key_prefix: "sk-abc", kind: "secret",
  identity_issuer: null, identity_subject: null, identity_claims: null, weight: 100, max_in_flight: null,
  budget_tokens: null, budget_cost_usd: null, budget_period: null, budget_started_at: null,
  disabled: false, tracing_enabled: false, end_user_fairshare: false, background: true, created_at: "", updated_at: "",
};

describe("background priority on a key", () => {
  it("is saved through its own call when its switch changes", async () => {
    const obleth = mockObleth();
    const { saveKeySettingsAction } = await import("./actions");
    const on = new FormData();
    on.set("id", "k1");
    on.set("sections", "background");
    on.set("background", "on");
    expect(await saveKeySettingsAction(on)).toEqual({ ok: true });
    // An unchecked switch submits nothing: that is "off".
    const off = new FormData();
    off.set("id", "k1");
    off.set("sections", "background");
    expect(await saveKeySettingsAction(off)).toEqual({ ok: true });
    expect(obleth.setKeyBackground.mock.calls.map((c) => [c[0], c[1]])).toEqual([["k1", true], ["k1", false]]);
    expect(obleth.setKeyEndUserFairshare).not.toHaveBeenCalled();
  });

  it("is left alone when another section is saved", async () => {
    const obleth = mockObleth();
    const { saveKeySettingsAction } = await import("./actions");
    const fd = new FormData();
    fd.set("id", "k1");
    fd.set("sections", "tracing");
    fd.set("tracing_enabled", "on");
    await saveKeySettingsAction(fd);
    expect(obleth.setKeyBackground).not.toHaveBeenCalled();
  });

  it("is kept when the key is replaced", async () => {
    const obleth = mockObleth({
      listKeys: vi.fn().mockResolvedValue([oldKey]),
      createKey: vi.fn().mockResolvedValue({ secret: "sk-new", key: { ...oldKey, id: "k2", background: false } }),
    });
    const { replaceKeyAction } = await import("./actions");
    const result = await replaceKeyAction("k1");
    expect(result.ok).toBe(true);
    expect(obleth.setKeyBackground).toHaveBeenCalledWith("k2", true, { auditActor: "admin@example.com" });
  });
});
