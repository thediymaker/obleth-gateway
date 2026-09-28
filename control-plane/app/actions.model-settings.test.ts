import { afterEach, describe, expect, it, vi } from "vitest";
afterEach(() => vi.resetModules());

// Same mocking approach as the other actions tests: actions.ts pulls in
// "next/cache", "@/lib/obleth" and "@/lib/auth/roles", none of which run
// under vitest.
function mockAdmin() {
  vi.doMock("@/lib/auth/roles", () => ({
    requireAdmin: async () => ({ id: "admin-1", email: "admin@example.com", role: "admin", status: "active", tenantId: null }),
  }));
  vi.doMock("next/cache", () => ({ revalidatePath: vi.fn(), updateTag: vi.fn() }));
}

const model = (over: Record<string, unknown> = {}) => ({
  id: "m-1", model_name: "m", description: "old", upstream_model: "up", api_base: "http://a", api_key_set: true,
  model_type: "chat", quantization: "unknown", aliases: [],
  input_cost_per_token: 0, output_cost_per_token: 0, cost_per_image: 0, cost_per_audio_second: 0, cost_per_character: 0, cost_per_video: 0,
  energy_slots_per_node: 0, route_bias: 1, auto_eligible: true, draft_model: "", verify_api_base: "", verify_upstream_model: "",
  context_window: 8192, admission_weight: 100, max_in_flight: 16, capacity_mode: "static", capacity_tuned_at: null,
  supports_function_calling: true, supports_system_messages: true, supports_response_schema: false, supports_tool_choice: true, supports_vision: false,
  enabled: false, cache_enabled: false, cache_ttl_secs: 300, request_timeout_secs: null, max_retries: 1, retry_backoff_ms: 200,
  endpoint_selection_mode: "failover", debug_diagnostics: false,
  tags: ["coding:3"], boons: ["compression"], tool_servers: ["docs"], created_at: "", updated_at: "",
  ...over,
});

class OblethApiError extends Error {}

function gateway(current = model(), over: Record<string, unknown> = {}) {
  const obleth = {
    listModels: vi.fn().mockResolvedValue([current]),
    updateModel: vi.fn().mockResolvedValue({}),
    validateModel: vi.fn().mockResolvedValue({ warnings: [] }),
    setModelCache: vi.fn().mockResolvedValue({}),
    setModelReliability: vi.fn().mockResolvedValue({}),
    setModelHealthConfig: vi.fn().mockResolvedValue({}),
    setModelCapacity: vi.fn().mockResolvedValue({}),
    setModelCapacityMode: vi.fn().mockResolvedValue({}),
    setModelCollections: vi.fn().mockResolvedValue({}),
    createModel: vi.fn().mockResolvedValue({ id: "new-1", model_name: "fresh" }),
    activateModel: vi.fn(),
    checkModelHealth: vi.fn().mockResolvedValue({}),
    deleteModel: vi.fn().mockResolvedValue(undefined),
    ...over,
  };
  vi.doMock("@/lib/obleth", () => ({ obleth, CACHE_TAGS: new Proxy({}, { get: () => "tag" }), OblethApiError }));
  return obleth;
}

function form(fields: Record<string, string | string[]>) {
  const fd = new FormData();
  fd.set("id", "m-1");
  for (const [k, v] of Object.entries(fields)) for (const one of [v].flat()) fd.append(k, one);
  return fd;
}

describe("saving a model's settings", () => {
  it("writes only the sections that changed", async () => {
    mockAdmin();
    const obleth = gateway();
    const { saveModelSettingsAction } = await import("./actions");
    const result = await saveModelSettingsAction(form({ sections: "cache", cache_enabled: "on", cache_ttl_secs: "60", description: "ignored" }));
    expect(result).toEqual({ ok: true });
    expect(obleth.setModelCache).toHaveBeenCalledWith("m-1", true, 60, { auditActor: "admin@example.com" });
    expect(obleth.updateModel).not.toHaveBeenCalled();
    expect(obleth.setModelReliability).not.toHaveBeenCalled();
  });

  it("keeps the route switch, the stored key, and every group the form leaves out", async () => {
    mockAdmin();
    const obleth = gateway();
    const { saveModelSettingsAction } = await import("./actions");
    await saveModelSettingsAction(form({ sections: "model", description: "new", api_key: "" }));
    const body = obleth.updateModel.mock.calls[0][1];
    expect(body.description).toBe("new");
    expect(body.enabled).toBe(false);
    expect("api_key" in body).toBe(false);
    // No has_tags / has_capabilities / has_routing: the stored values carry through.
    expect(body.tags).toEqual(["coding:3"]);
    expect(body.boons).toEqual(["compression"]);
    expect(body.tool_servers).toEqual(["docs"]);
    expect(body.supports_function_calling).toBe(true);
    expect(body.auto_eligible).toBe(true);
  });

  it("applies a cleared group when its marker is there", async () => {
    mockAdmin();
    const obleth = gateway();
    const { saveModelSettingsAction } = await import("./actions");
    await saveModelSettingsAction(form({ sections: "model", has_capabilities: "1", has_routing: "1", has_tags: "1" }));
    const body = obleth.updateModel.mock.calls[0][1];
    expect(body.boons).toEqual([]);
    expect(body.tool_servers).toEqual([]);
    expect(body.supports_function_calling).toBe(false);
    expect(body.auto_eligible).toBe(false);
    expect(body.tags).toEqual([]);
  });

  it("sends a new key only when one was typed", async () => {
    mockAdmin();
    const obleth = gateway();
    const { saveModelSettingsAction } = await import("./actions");
    await saveModelSettingsAction(form({ sections: "model", api_key: "sk-new" }));
    expect(obleth.updateModel.mock.calls[0][1].api_key).toBe("sk-new");
  });

  it("checks the upstream again only when the connection moved", async () => {
    mockAdmin();
    const obleth = gateway(model(), { validateModel: vi.fn().mockResolvedValue({ warnings: ["up is not listed"] }) });
    const { saveModelSettingsAction } = await import("./actions");
    expect(await saveModelSettingsAction(form({ sections: "model", description: "x" }))).toEqual({ ok: true });
    expect(obleth.validateModel).not.toHaveBeenCalled();
    expect(await saveModelSettingsAction(form({ sections: "model", api_base: "http://b" }))).toEqual({ ok: true, warnings: ["up is not listed"] });
  });

  it("clears the slot cap when the field is emptied, and switches mode", async () => {
    mockAdmin();
    const obleth = gateway();
    const { saveModelSettingsAction } = await import("./actions");
    await saveModelSettingsAction(form({ sections: "capacity", max_in_flight: "", capacity_mode: "tuned" }));
    expect(obleth.setModelCapacity).toHaveBeenCalledWith("m-1", null, expect.anything());
    expect(obleth.setModelCapacityMode).toHaveBeenCalledWith("m-1", "tuned", undefined, expect.anything());
  });

  it("stops at the first refusal and says what was already saved", async () => {
    mockAdmin();
    gateway(model(), { setModelReliability: vi.fn().mockRejectedValue(new OblethApiError("max_retries must be at most 5")) });
    const { saveModelSettingsAction } = await import("./actions");
    const result = await saveModelSettingsAction(form({ sections: "health,reliability,cache", max_retries: "9", cache_enabled: "on" }));
    expect(result).toEqual({ ok: false, error: "Delivery: max_retries must be at most 5", saved: ["cache"] });
  });

  it("replaces knowledge attachments only when the current set loaded", async () => {
    mockAdmin();
    const obleth = gateway();
    const { saveModelSettingsAction } = await import("./actions");
    await saveModelSettingsAction(form({ sections: "knowledge", knowledge_collection: ["c1", "c2"] }));
    expect(obleth.setModelCollections).not.toHaveBeenCalled();
    await saveModelSettingsAction(form({ sections: "knowledge", knowledge_loaded: "1", knowledge_collection: ["c1", "c2"] }));
    expect(obleth.setModelCollections).toHaveBeenCalledWith("m-1", ["c1", "c2"], expect.anything());
  });
});

describe("the serving switch", () => {
  it("sends the whole model with only the switch changed", async () => {
    mockAdmin();
    const obleth = gateway(model({ enabled: true }));
    const { setModelEnabledAction } = await import("./actions");
    expect(await setModelEnabledAction("m-1", false)).toEqual({ ok: true });
    const body = obleth.updateModel.mock.calls[0][1];
    expect(body.enabled).toBe(false);
    expect(body.tags).toEqual(["coding:3"]);
    expect(body.max_in_flight).toBe(16);
  });
});

describe("creating a model", () => {
  function createForm() {
    const fd = new FormData();
    fd.set("model_name", "Fresh");
    fd.set("upstream_model", "org/Fresh");
    fd.set("api_base", "http://fresh:8000/v1");
    fd.set("endpoint_mode", "static");
    return fd;
  }

  it("creates it switched off and turns it on when the first check passes", async () => {
    mockAdmin();
    const obleth = gateway(model(), {
      activateModel: vi.fn().mockResolvedValue({ enabled: true, activated: true, detail: { summary: { status: "healthy", last_message: null }, checks: [] } }),
    });
    const { createModelAction } = await import("./actions");
    const result = await createModelAction(createForm());
    expect(obleth.createModel.mock.calls[0][0]).toMatchObject({ model_name: "fresh", enabled: false });
    expect(obleth.activateModel).toHaveBeenCalledWith("new-1", { auditActor: "admin@example.com" });
    expect(result).toMatchObject({ ok: true, model: { id: "new-1", name: "fresh" }, enabled: true, check: null });
  });

  it("leaves it off with the check's reason when the check fails", async () => {
    mockAdmin();
    gateway(model(), {
      activateModel: vi.fn().mockResolvedValue({ enabled: false, activated: false, detail: { summary: { status: "unhealthy", last_message: "HTTP 404: model not found" }, checks: [] } }),
    });
    const { createModelAction } = await import("./actions");
    expect(await createModelAction(createForm())).toMatchObject({ ok: true, enabled: false, check: "HTTP 404: model not found" });
  });

  it("still reports the model when the check itself can't run", async () => {
    mockAdmin();
    gateway(model(), { activateModel: vi.fn().mockRejectedValue(new OblethApiError("gateway timeout")) });
    const { createModelAction } = await import("./actions");
    expect(await createModelAction(createForm())).toMatchObject({ ok: true, enabled: false, check: "The first health check could not run: gateway timeout" });
  });
});

describe("bulk actions", () => {
  it("names each model that failed and counts the rest", async () => {
    mockAdmin();
    gateway(model(), {
      listModels: vi.fn().mockResolvedValue([model({ id: "a", model_name: "alpha" }), model({ id: "b", model_name: "beta" }), model({ id: "c", model_name: "gamma" })]),
      deleteModel: vi.fn().mockImplementation(async (id: string) => { if (id === "b") throw new OblethApiError("cache eviction failed"); }),
    });
    const { deleteModelsAction } = await import("./actions");
    expect(await deleteModelsAction(["a", "b"])).toEqual({ done: 1, failed: [{ name: "beta", error: "cache eviction failed" }] });
  });

  it("skips models already in the asked-for state", async () => {
    mockAdmin();
    const obleth = gateway(model(), {
      listModels: vi.fn().mockResolvedValue([model({ id: "a", enabled: true }), model({ id: "b", enabled: false })]),
    });
    const { setModelsEnabledAction } = await import("./actions");
    await setModelsEnabledAction(["a", "b"], true);
    expect(obleth.updateModel).toHaveBeenCalledTimes(1);
    expect(obleth.updateModel.mock.calls[0][0]).toBe("b");
  });
});
