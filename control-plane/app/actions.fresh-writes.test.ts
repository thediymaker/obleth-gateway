import { beforeEach, describe, expect, it, vi } from "vitest";

// A write carries only what it changes: the gateway keeps every field it is
// not sent, so a copy of the model taken from an earlier read must never
// ride along and revert what changed in between.
const model = { id: "m1", model_name: "glm", upstream_model: "org/glm", api_base: "http://up", enabled: true, description: "d", tool_servers: ["a"], boons: [] };

function setup(overrides: Record<string, unknown> = {}) {
  vi.resetModules();
  vi.doMock("next/cache", () => ({ revalidatePath: vi.fn() }));
  vi.doMock("@/lib/auth/roles", () => ({ requireAdmin: async () => ({ email: "admin@example.com" }) }));
  const obleth = { getModel: vi.fn(async () => model), updateModel: vi.fn(async () => model), ...overrides };
  vi.doMock("@/lib/obleth", async () => ({ ...(await vi.importActual<object>("@/lib/obleth")), obleth }));
  return obleth;
}

beforeEach(() => vi.clearAllMocks());

describe("model writes", () => {
  it("send only the changed field with the two required identifiers", async () => {
    const obleth = setup();
    const { setModelEnabledAction } = await import("./actions");
    expect(await setModelEnabledAction("m1", false)).toEqual({ ok: true });
    expect(obleth.getModel).toHaveBeenCalledWith("m1");
    expect(obleth.updateModel).toHaveBeenCalledWith("m1", { upstream_model: "org/glm", api_base: "http://up", enabled: false }, { auditActor: "admin@example.com" });
  });

  it("read the model itself, not a list, and report a model that is gone", async () => {
    const { OblethApiError } = await vi.importActual<typeof import("@/lib/obleth")>("@/lib/obleth");
    const obleth = setup({ getModel: vi.fn(async () => { throw new OblethApiError(404, "no such model", "/models/m1"); }) });
    const { setModelEnabledAction } = await import("./actions");
    expect(await setModelEnabledAction("m1", false)).toEqual({ ok: false, error: "Model not found." });
    expect(obleth.updateModel).not.toHaveBeenCalled();
  });

  it("grant a tool server without re-sending the rest of the model", async () => {
    const obleth = setup();
    const { setModelToolServerAction } = await import("./actions");
    expect(await setModelToolServerAction("m1", "b", true)).toEqual({ ok: true });
    expect(obleth.updateModel).toHaveBeenCalledWith("m1", { upstream_model: "org/glm", api_base: "http://up", tool_servers: ["a", "b"] }, { auditActor: "admin@example.com" });
  });
});
