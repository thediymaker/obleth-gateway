import { afterEach, describe, expect, it, vi } from "vitest";
afterEach(() => vi.resetModules());

// Mirrors the mocking approach in actions.model-tags.test.ts: actions.ts pulls
// in "next/cache", "@/lib/obleth" and "@/lib/auth/roles", none of which can run
// under vitest.
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
  api_key_set: false,
  model_type: "chat",
  input_cost_per_token: 0,
  output_cost_per_token: 0,
  cost_per_image: 0,
  cost_per_audio_second: 0,
  cost_per_character: 0,
  cost_per_video: 0,
  energy_slots_per_node: 1,
  route_bias: 1,
  auto_eligible: true,
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

async function connectionUpdate(current: Record<string, unknown>, fd: FormData) {
  mockAdmin();
  const updateModel = vi.fn().mockResolvedValue({});
  vi.doMock("@/lib/obleth", () => ({
    obleth: {
      listModels: vi.fn().mockResolvedValue([current]),
      updateModel,
      listUpstreamModels: vi.fn().mockResolvedValue([]),
    },
    CACHE_TAGS: new Proxy({}, { get: () => "tag" }),
    OblethApiError: class OblethApiError extends Error {},
  }));
  const { updateModelConnectionAction } = await import("./actions");
  await updateModelConnectionAction(null, fd);
  return updateModel;
}

function connectionForm(over: Record<string, string> = {}) {
  const fd = new FormData();
  fd.set("id", "m-1");
  fd.set("upstream_model", "up");
  fd.set("api_base", "http://a");
  for (const [k, v] of Object.entries(over)) fd.set(k, v);
  return fd;
}

describe("upstream headers round-trip through the model form", () => {
  it("sends the textarea as a write, with a blank value meaning keep", async () => {
    const updateModel = await connectionUpdate(
      model({ upstream_header_names: ["x-upstream-token"] }),
      connectionForm({
        upstream_headers: "x-upstream-token:\nx-routing-hint: sticky",
      }),
    );
    expect(updateModel.mock.calls[0][1].upstream_headers).toEqual({
      "x-upstream-token": null,
      "x-routing-hint": "sticky",
    });
  });

  it("clears every header when the operator empties the field", async () => {
    const updateModel = await connectionUpdate(
      model({ upstream_header_names: ["x-upstream-token"] }),
      connectionForm({ upstream_headers: "" }),
    );
    expect(updateModel.mock.calls[0][1].upstream_headers).toEqual({});
  });

  it("leaves the headers alone when the form has no headers field", async () => {
    // Omitted means keep: a save that never rendered the field must not
    // send an empty set and wipe what is stored.
    const updateModel = await connectionUpdate(
      model({ upstream_header_names: ["x-upstream-token"] }),
      connectionForm(),
    );
    expect("upstream_headers" in updateModel.mock.calls[0][1]).toBe(false);
    expect("upstream_header_names" in updateModel.mock.calls[0][1]).toBe(false);
  });
});
