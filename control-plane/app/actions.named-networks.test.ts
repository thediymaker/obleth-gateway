import { afterEach, describe, expect, it, vi } from "vitest";
afterEach(() => vi.resetModules());

// Same mocking approach as actions.key-networks.test.ts.
function mockObleth() {
  vi.doMock("@/lib/auth/roles", () => ({
    requireAdmin: async () => ({ id: "admin-1", email: "admin@example.com", role: "admin", status: "active", tenantId: null }),
  }));
  vi.doMock("next/cache", () => ({ revalidatePath: vi.fn(), updateTag: vi.fn() }));
  const obleth = { setNamedNetworks: vi.fn().mockResolvedValue({}) };
  vi.doMock("@/lib/obleth", () => ({
    obleth,
    CACHE_TAGS: new Proxy({}, { get: () => "tag" }),
    OblethApiError: class OblethApiError extends Error {},
  }));
  return obleth;
}

function form(list: string) {
  const f = new FormData();
  f.set("sections", "networks");
  f.set("networks.list", list);
  return f;
}

describe("saving named networks from Settings", () => {
  it("sends the cleaned list with who changed it", async () => {
    const obleth = mockObleth();
    const { saveSettingsAction } = await import("./actions");
    const res = await saveSettingsAction(form(JSON.stringify([{ network: " 10.0.0.0/8 ", name: " Campus " }, { network: "", name: "" }])));
    expect(res).toEqual({ ok: true });
    expect(obleth.setNamedNetworks).toHaveBeenCalledWith([{ network: "10.0.0.0/8", name: "Campus" }], { auditActor: "admin@example.com" });
  });

  it("refuses a row without a name before calling the gateway", async () => {
    const obleth = mockObleth();
    const { saveSettingsAction } = await import("./actions");
    const res = await saveSettingsAction(form(JSON.stringify([{ network: "10.0.0.0/8", name: "" }])));
    expect(res).toMatchObject({ ok: false, error: "Networks: 10.0.0.0/8 needs a name." });
    expect(obleth.setNamedNetworks).not.toHaveBeenCalled();
  });

  it("refuses a list it can't read", async () => {
    const obleth = mockObleth();
    const { saveSettingsAction } = await import("./actions");
    const res = await saveSettingsAction(form("{"));
    expect(res).toMatchObject({ ok: false });
    expect(obleth.setNamedNetworks).not.toHaveBeenCalled();
  });
});
