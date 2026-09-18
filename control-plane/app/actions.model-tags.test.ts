import { afterEach, describe, expect, it, vi } from "vitest";
afterEach(() => vi.resetModules());

// actions.ts imports from:
//   "next/cache"       -> revalidatePath, updateTag
//   "@/lib/obleth"      -> CACHE_TAGS, obleth, OblethApiError
//   "@/lib/auth/roles"  -> requireAdmin

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

const model = (over: Record<string, unknown> = {}) => ({
  id: "m-1",
  model_name: "m",
  description: "",
  upstream_model: "up",
  api_base: "http://a",
  api_key: null,
  model_type: "chat",
  input_cost_per_token: 0,
  output_cost_per_token: 0,
  cost_per_image: 0,
  cost_per_audio_second: 0,
  cost_per_character: 0,
  energy_slots_per_node: 1,
  route_bias: 1,
  context_window: 8192,
  admission_weight: 1,
  max_in_flight: null,
  capacity_mode: "static",
  capacity_tuned_at: null,
  supports_function_calling: true,
  supports_system_messages: true,
  supports_response_schema: false,
  supports_tool_choice: false,
  supports_vision: false,
  enabled: true,
  cache_enabled: false,
  cache_ttl_secs: 0,
  request_timeout_secs: null,
  max_retries: 0,
  retry_backoff_ms: 0,
  endpoint_selection_mode: "priority",
  debug_diagnostics: false,
  tags: [] as string[],
  boons: [] as string[],
  tool_servers: [] as string[],
  created_at: "",
  updated_at: "",
  ...over,
});

describe("model tag strength levels round-trip through updateModelCapabilitiesAction", () => {
  it("serializes Auto bare, and every explicit level — including 1 — with a suffix", async () => {
    mockAdmin();
    const updateModel = vi.fn().mockResolvedValue({});
    vi.doMock("@/lib/obleth", () => ({
      obleth: { listModels: vi.fn().mockResolvedValue([model()]), updateModel },
      CACHE_TAGS: new Proxy({}, { get: () => "tag" }),
      OblethApiError: class OblethApiError extends Error {},
    }));
    const { updateModelCapabilitiesAction } = await import("./actions");
    const fd = new FormData();
    fd.set("id", "m-1");
    // Auto (0): saved bare, the level derives from cost rank under hybrid
    // tier sourcing.
    fd.set("tag_coding", "on");
    fd.set("tag_level_coding", "0");
    fd.set("tag_math", "on");
    fd.set("tag_level_math", "3");
    // Explicit level 1 keeps its suffix: "pinned weak on purpose" is a
    // different statement from "derive it for me".
    fd.set("tag_general", "on");
    fd.set("tag_level_general", "1");
    await updateModelCapabilitiesAction(null, fd);
    expect(updateModel).toHaveBeenCalledWith(
      "m-1",
      expect.objectContaining({ tags: expect.arrayContaining(["coding", "math:3", "general:1"]) }),
      expect.anything(),
    );
    const body = updateModel.mock.calls[0][1];
    expect(body.tags).toEqual(["coding", "math:3", "general:1"]);
  });

  it("sets supports_vision from a suffixed vision:2 tag", async () => {
    mockAdmin();
    const updateModel = vi.fn().mockResolvedValue({});
    vi.doMock("@/lib/obleth", () => ({
      obleth: { listModels: vi.fn().mockResolvedValue([model()]), updateModel },
      CACHE_TAGS: new Proxy({}, { get: () => "tag" }),
      OblethApiError: class OblethApiError extends Error {},
    }));
    const { updateModelCapabilitiesAction } = await import("./actions");
    const fd = new FormData();
    fd.set("id", "m-1");
    fd.set("tag_vision", "on");
    fd.set("tag_level_vision", "2");
    await updateModelCapabilitiesAction(null, fd);
    const body = updateModel.mock.calls[0][1];
    expect(body.tags).toEqual(["vision:2"]);
    expect(body.supports_vision).toBe(true);
  });

  it("round-trips an untouched model's declared tags byte-identical", async () => {
    mockAdmin();
    const updateModel = vi.fn().mockResolvedValue({});
    vi.doMock("@/lib/obleth", () => ({
      obleth: { listModels: vi.fn().mockResolvedValue([model({ tags: ["coding:3", "math"] })]), updateModel },
      CACHE_TAGS: new Proxy({}, { get: () => "tag" }),
      OblethApiError: class OblethApiError extends Error {},
    }));
    const { updateModelCapabilitiesAction } = await import("./actions");
    // Field values exactly as ChatCapabilityFields would submit them, loaded
    // from a model with ["coding:3", "math"] and never touched: the bare tag
    // loads as Auto (0) and must save bare again.
    const fd = new FormData();
    fd.set("id", "m-1");
    fd.set("tag_coding", "on");
    fd.set("tag_level_coding", "3");
    fd.set("tag_math", "on");
    fd.set("tag_level_math", "0");
    await updateModelCapabilitiesAction(null, fd);
    const body = updateModel.mock.calls[0][1];
    expect(body.tags).toEqual(["coding:3", "math"]);
  });

  it.each([
    ["-2", "coding"], // below range clamps up to Auto, which serializes bare
    ["9", "coding:3"], // above range clamps down to 3
    ["x", "coding"], // unparseable falls back to Auto, which serializes bare
  ])("clamps a malformed submitted level (%s) into 0..3 without dropping the tag", async (rawLevel, expectedTag) => {
    mockAdmin();
    const updateModel = vi.fn().mockResolvedValue({});
    vi.doMock("@/lib/obleth", () => ({
      obleth: { listModels: vi.fn().mockResolvedValue([model()]), updateModel },
      CACHE_TAGS: new Proxy({}, { get: () => "tag" }),
      OblethApiError: class OblethApiError extends Error {},
    }));
    const { updateModelCapabilitiesAction } = await import("./actions");
    const fd = new FormData();
    fd.set("id", "m-1");
    fd.set("tag_coding", "on");
    fd.set("tag_level_coding", rawLevel);
    await updateModelCapabilitiesAction(null, fd);
    const body = updateModel.mock.calls[0][1];
    expect(body.tags).toEqual([expectedTag]);
  });
});
