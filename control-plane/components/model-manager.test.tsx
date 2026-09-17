import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ChatCapabilityFields } from "./model-manager";
import { TooltipProvider } from "./ui/tooltip";
import type { ModelRoute } from "@/lib/obleth";

// model-manager.tsx statically imports every model server action; stub them
// so importing the module doesn't pull in "@/app/actions" (a "use server"
// file with its own heavy dependency graph) for real.
vi.mock("@/app/actions", () => ({
  applyAutotuneCapacityAction: vi.fn(),
  autotuneModelAction: vi.fn(),
  checkModelHealthAction: vi.fn(),
  createModelAction: vi.fn(),
  createModelEndpointAction: vi.fn(),
  deleteModelAction: vi.fn(),
  deleteModelEndpointAction: vi.fn(),
  applyModelManifestAction: vi.fn(),
  setModelCacheAction: vi.fn(),
  setModelCapacityAction: vi.fn(),
  setModelCapacityModeAction: vi.fn(),
  setModelHealthConfigAction: vi.fn(),
  restartReplicaAction: vi.fn(),
  setModelReliabilityAction: vi.fn(),
  setModelWeightAction: vi.fn(),
  updateModelConnectionAction: vi.fn(),
  updateModelCapabilitiesAction: vi.fn(),
  updateModelEndpointAction: vi.fn(),
}));

const model = (over: Partial<ModelRoute> = {}): ModelRoute => ({
  id: "u", model_name: "m", description: "", upstream_model: "up", api_base: "http://a",
  api_key: null, model_type: "chat", input_cost_per_token: 0, output_cost_per_token: 0,
  cost_per_image: 0, cost_per_audio_second: 0, cost_per_character: 0, energy_slots_per_node: 1,
  route_bias: 1,
  auto_eligible: true,
  draft_model: "",
  verify_api_base: "",
  verify_upstream_model: "",
  context_window: 8192, admission_weight: 1, max_in_flight: null, capacity_mode: "static",
  capacity_tuned_at: null, supports_function_calling: true, supports_system_messages: true,
  supports_response_schema: false, supports_tool_choice: false, supports_vision: false,
  enabled: true, cache_enabled: false, cache_ttl_secs: 0, request_timeout_secs: null,
  max_retries: 0, retry_backoff_ms: 0, endpoint_selection_mode: "priority", debug_diagnostics: false,
  tags: [], boons: [], tool_servers: [], created_at: "", updated_at: "",
  ...over,
});

let root: Root;
let host: HTMLDivElement;

async function renderFields(m: ModelRoute, boonBlockers: Record<string, string> = {}) {
  await act(async () => {
    root.render(
      <TooltipProvider>
        <form>
          <ChatCapabilityFields model={m} mcpServers={[]} boonBlockers={boonBlockers} />
        </form>
      </TooltipProvider>,
    );
  });
}

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

describe("routing tag strength levels", () => {
  it("renders a suffixed tag checked with its level selected (regression: the dashboard used to drop levelled tags)", async () => {
    await renderFields(model({ tags: ["coding:3"] }));
    const checkbox = host.querySelector<HTMLInputElement>('[name="tag_coding"]')!;
    expect(checkbox.checked).toBe(true);
    const level3 = host.querySelector<HTMLInputElement>('[name="tag_level_coding"][value="3"]')!;
    expect(level3.checked).toBe(true);
  });

  it("renders a bare tag checked at Auto, and an explicit :1 pinned at 1", async () => {
    // Bare = "derive my level from cost rank" (Auto, value 0); tag:1 = pinned
    // weak on purpose. The picker must show the operator which one is stored.
    await renderFields(model({ tags: ["coding", "math:1"] }));
    expect(host.querySelector<HTMLInputElement>('[name="tag_coding"]')!.checked).toBe(true);
    expect(host.querySelector<HTMLInputElement>('[name="tag_level_coding"][value="0"]')!.checked).toBe(true);
    expect(host.querySelector<HTMLInputElement>('[name="tag_level_math"][value="1"]')!.checked).toBe(true);
  });

  it("shows no level control for an unchecked tag", async () => {
    await renderFields(model({ tags: [] }));
    expect(host.querySelector('[name="tag_level_coding"]')).toBeNull();
  });

  it("checks vision from supports_vision alone, with no explicit vision tag", async () => {
    await renderFields(model({ supports_vision: true, tags: [] }));
    expect(host.querySelector<HTMLInputElement>('[name="tag_vision"]')!.checked).toBe(true);
  });

  it.each([
    ["coding:0", 1], // below range clamps up (an explicit suffix stays declared)
    ["coding:9", 3], // above range clamps down
    ["coding:x", 1], // unparseable falls back to 1
  ] as const)("clamps a malformed stored level (%s) into 1..3 without dropping the tag", async (raw, expectedLevel) => {
    await renderFields(model({ tags: [raw] }));
    expect(host.querySelector<HTMLInputElement>('[name="tag_coding"]')!.checked).toBe(true);
    expect(host.querySelector<HTMLInputElement>(`[name="tag_level_coding"][value="${expectedLevel}"]`)!.checked).toBe(true);
  });

  it("round-trips an untouched form back to the field values the model was loaded with", async () => {
    await renderFields(model({ tags: ["coding:3", "math"] }));
    const data = new FormData(host.querySelector("form")!);
    expect(data.get("tag_coding")).toBe("on");
    expect(data.get("tag_level_coding")).toBe("3");
    expect(data.get("tag_math")).toBe("on");
    // Bare tag loads as Auto (0), which tagsFromForm saves bare again.
    expect(data.get("tag_level_math")).toBe("0");
    expect(data.get("tag_general")).toBeNull();
  });
});

describe("boons that are not configured globally", () => {
  const OFF = "it is switched off in Settings → Boons.";

  it("refuses a new grant for a boon whose global switch is off", async () => {
    await renderFields(model({ boons: [] }), { image_generation: OFF });
    const checkbox = host.querySelector<HTMLInputElement>('[name="boon_image_generation"]')!;
    expect(checkbox.disabled).toBe(true);
    expect(host.textContent).toContain("can’t be granted");
    expect(host.textContent).toContain(OFF);
  });

  it("leaves an existing grant operable so an unrelated save can't silently revoke it", async () => {
    // A disabled checkbox submits nothing, so disabling a granted boon would
    // drop it from the form the next time capabilities were saved.
    await renderFields(model({ boons: ["image_generation"] }), { image_generation: OFF });
    const checkbox = host.querySelector<HTMLInputElement>('[name="boon_image_generation"]')!;
    expect(checkbox.disabled).toBe(false);
    expect(checkbox.checked).toBe(true);
    expect(new FormData(host.querySelector("form")!).get("boon_image_generation")).toBe("on");
    expect(host.textContent).toContain("is granted but inactive");
  });

  it("gates the controlled chips (knowledge, speculation) the same way", async () => {
    await renderFields(model({ boons: [] }), {
      knowledge: "retrieval is switched off in Settings → Knowledge.",
      speculation: OFF,
    });
    expect(host.querySelector<HTMLInputElement>('[name="boon_knowledge"]')!.disabled).toBe(true);
    expect(host.querySelector<HTMLInputElement>('[name="boon_speculation"]')!.disabled).toBe(true);
  });

  it("says nothing when every boon is configured", async () => {
    await renderFields(model({ boons: ["compression"] }));
    expect(host.querySelector<HTMLInputElement>('[name="boon_compression"]')!.disabled).toBe(false);
    expect(host.textContent).not.toContain("can’t be granted");
    expect(host.textContent).not.toContain("granted but inactive");
  });
});
