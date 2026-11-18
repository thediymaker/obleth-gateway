import { afterEach, describe, expect, it, vi } from "vitest";

afterEach(() => vi.resetModules());

class OblethApiError extends Error {
  constructor(
    public readonly status: number,
    message: string,
    public readonly path: string,
  ) {
    super(message);
  }
}

function setup(obleth: Record<string, unknown>) {
  const revalidatePath = vi.fn();
  vi.doMock("@/lib/auth/roles", () => ({
    requireAdmin: async () => ({ id: "admin-1", email: "admin@example.com", role: "admin", status: "active", tenantId: null }),
  }));
  vi.doMock("next/cache", () => ({ revalidatePath }));
  vi.doMock("@/lib/obleth", () => ({
    obleth,
    OblethApiError,
  }));
  return { revalidatePath };
}

const members = { none: "acme-7b-instruct", low: "acme-7b-think", medium: "acme-7b-think", high: "acme-7b-think" };

describe("setModelReasoningAction", () => {
  it("writes the map whole and refreshes the model lists", async () => {
    const setModelReasoning = vi.fn().mockResolvedValue({});
    const { revalidatePath } = setup({ setModelReasoning });
    const { setModelReasoningAction } = await import("./actions");
    const body = { mode: "unsupported" as const };
    expect(await setModelReasoningAction("m1", body)).toEqual({ ok: true });
    expect(setModelReasoning).toHaveBeenCalledWith("m1", body, { auditActor: "admin@example.com" });
    expect(revalidatePath).toHaveBeenCalledWith("/models");
  });

  it("refuses a map with levels missing without asking the gateway", async () => {
    const setModelReasoning = vi.fn();
    setup({ setModelReasoning });
    const { setModelReasoningAction } = await import("./actions");
    const result = await setModelReasoningAction("m1", { mode: "mapped", levels: { low: { send: {} } } });
    expect(result).toEqual({ ok: false, error: "Set all four levels to translate them. Not set: none, medium, high." });
    expect(setModelReasoning).not.toHaveBeenCalled();
  });

  it("passes the gateway's 400 message through", async () => {
    setup({ setModelReasoning: vi.fn().mockRejectedValue(new OblethApiError(400, "`e` is a `embedding` model; only chat models take reasoning effort", "/x")) });
    const { setModelReasoningAction } = await import("./actions");
    expect(await setModelReasoningAction("m1", { mode: "unsupported" })).toEqual({ ok: false, error: "`e` is a `embedding` model; only chat models take reasoning effort" });
  });
});

describe("model family actions", () => {
  it("creates without an id and replaces with one, trimmed", async () => {
    const createModelFamily = vi.fn().mockResolvedValue({});
    const updateModelFamily = vi.fn().mockResolvedValue({});
    const { revalidatePath } = setup({ createModelFamily, updateModelFamily });
    const { saveModelFamilyAction } = await import("./actions");
    expect(await saveModelFamilyAction(null, { name: " acme-7b ", description: " d ", default_level: "none", members })).toEqual({ ok: true });
    expect(createModelFamily).toHaveBeenCalledWith({ name: "acme-7b", description: "d", default_level: "none", members }, { auditActor: "admin@example.com" });
    expect(await saveModelFamilyAction("f1", { name: "acme-7b", default_level: "high", members })).toEqual({ ok: true });
    expect(updateModelFamily).toHaveBeenCalledWith("f1", { name: "acme-7b", description: "", default_level: "high", members }, { auditActor: "admin@example.com" });
    expect(revalidatePath).toHaveBeenCalledWith("/models");
  });

  it("passes the gateway's refusal through", async () => {
    setup({ createModelFamily: vi.fn().mockRejectedValue(new OblethApiError(400, "a model family named 'acme-7b' already exists", "/x")) });
    const { saveModelFamilyAction } = await import("./actions");
    expect(await saveModelFamilyAction(null, { name: "acme-7b", default_level: "none", members })).toEqual({ ok: false, error: "a model family named 'acme-7b' already exists" });
  });

  it("deletes and refreshes the list either way", async () => {
    const deleteModelFamily = vi.fn().mockRejectedValue(new OblethApiError(404, "not found", "/x"));
    const { revalidatePath } = setup({ deleteModelFamily });
    const { deleteModelFamilyAction } = await import("./actions");
    expect(await deleteModelFamilyAction("f1")).toEqual({ ok: false, error: "not found" });
    expect(deleteModelFamily).toHaveBeenCalledWith("f1", { auditActor: "admin@example.com" });
    expect(revalidatePath).toHaveBeenCalledWith("/models");
  });
});
