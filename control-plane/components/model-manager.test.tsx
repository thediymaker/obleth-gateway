import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { setModelCapacityDiscoveryAction, setModelCapacityModeAction } from "@/app/actions";
import {
  CapacityDiscoveryPanel,
  CapacityDiscoveryStatus,
  capacityInFlight,
  CapacityModeToggle,
  ChatCapabilityFields,
} from "./model-manager";
import { TooltipProvider } from "./ui/tooltip";
import type { CapacityDiscoveryView, CapacityServicesView, ModelRoute } from "@/lib/obleth";

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
  setModelCapacityDiscoveryAction: vi.fn(async () => ({ ok: true })),
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
  api_key_set: false, model_type: "chat", quantization: "unknown", aliases: [],
  input_cost_per_token: 0, output_cost_per_token: 0,
  cost_per_image: 0, cost_per_audio_second: 0, cost_per_character: 0, cost_per_video: 0, energy_slots_per_node: 1,
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

describe("the discovered capacity mode", () => {
  const view = (over: Partial<CapacityDiscoveryView["models"][number]["status"]> = {}): CapacityDiscoveryView => ({
    enabled: true,
    interval_secs: 15,
    namespaces: ["inference"],
    default_service: "{upstream_model}",
    replicas: 2,
    mode: "shared",
    models: [
      {
        model_id: "u",
        model_name: "m",
        enabled: true,
        static_max_in_flight: 6,
        enforced_max_in_flight: 16,
        cluster_in_flight: 11,
        in_flight: 4,
        status: {
          model_name: "m",
          source: "kubernetes",
          namespaces: ["inference", "batch"],
          namespace: "inference",
          service: "up",
          ready_replicas: 2,
          per_replica_max_in_flight: 8,
          per_replica_source: "configured",
          headroom: 1,
          derived_max_in_flight: 16,
          effective_max_in_flight: 16,
          state: "discovered",
          last_refresh: "2026-09-24T12:00:00Z",
          last_success: "2026-09-24T12:00:00Z",
          reason: null,
          ...over,
        },
      },
    ],
  });

  const services = (over: Partial<CapacityServicesView> = {}): CapacityServicesView => ({
    services: [
      { service: "up", namespace: "inference", ready: 2 },
      { service: "up-head", namespace: "inference", ready: 1 },
      { service: "other", namespace: "batch", ready: 0 },
    ],
    reason: null,
    errors: [],
    default_service: "up",
    default_match: { service: "up", namespace: "inference", ready: 2 },
    ...over,
  });

  async function renderPanel(m: ModelRoute, v: CapacityDiscoveryView, svc: CapacityServicesView = services()) {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
    client.setQueryData(["capacity-discovery"], v);
    client.setQueryData(["capacity-services", m.model_name], svc);
    await act(async () => {
      root.render(
        <QueryClientProvider client={client}>
          <TooltipProvider>
            <CapacityDiscoveryPanel model={m} />
          </TooltipProvider>
        </QueryClientProvider>,
      );
    });
  }

  it("offers static, tuned and discovered, and switches mode on click", async () => {
    await act(async () => {
      root.render(<CapacityModeToggle id="u" mode="static" />);
    });
    const buttons = [...host.querySelectorAll<HTMLButtonElement>('[aria-label="Capacity mode"] button')];
    expect(buttons.map((b) => b.textContent)).toEqual(["static", "tuned", "discovered"]);
    await act(async () => buttons[2].click());
    expect(setModelCapacityModeAction).toHaveBeenCalledWith("u", "discovered");
  });

  it("shows the kubernetes fields with the stored values, and the live derivation", async () => {
    await renderPanel(
      model({
        capacity_mode: "discovered",
        capacity_source: "kubernetes",
        capacity_namespace: "inference",
        capacity_service: "up-head",
        per_replica_max_in_flight: 8,
        capacity_headroom: 1.25,
      }),
      view(),
    );
    const input = (name: string) => host.querySelector<HTMLInputElement>(`input[name="${name}"]`);
    expect(input("capacity_namespace")!.value).toBe("inference");
    expect(input("capacity_service")!.value).toBe("up-head");
    expect(host.querySelector('[data-testid="service-chosen"]')!.textContent).toContain(
      "Using up-head in inference · 1 ready",
    );
    expect(input("per_replica_max_in_flight")!.value).toBe("8");
    expect(input("capacity_headroom")!.value).toBe("1.25");
    expect(host.querySelector('input[name="capacity_selector"]')).toBeNull();
    const text = host.textContent ?? "";
    expect(text).toContain("discovered");
    expect(host.querySelector('[data-testid="discovery-equation"]')!.textContent).toBe(
      "Live: 2 ready × 8 per replica = 16",
    );
    expect(text).toContain("Service up in inference");
    expect(text).toContain("8 (configured)");
    expect(text).toContain("16");
    expect(host.querySelector('[data-testid="capacity-in-flight"]')!.textContent).toBe(
      "11 of 16 (cluster-wide, 2 gateways)this gateway 4",
    );
    expect(text).not.toContain("replica's share");
  });

  it("labels the split and the fallback as this gateway's own limit", () => {
    const entry = { enforced_max_in_flight: 8, cluster_in_flight: null, in_flight: 3 };
    expect(capacityInFlight(entry, 16, "split", 2)).toEqual({
      value: "3 of 8",
      note: "this gateway's share, split across 2 gateways",
      fallback: false,
    });
    const fallback = capacityInFlight(entry, 16, "fallback", 2);
    expect(fallback.value).toBe("3 of 8");
    expect(fallback.fallback).toBe(true);
    expect(fallback.note).toContain("shared slots unavailable");
    expect(capacityInFlight({ ...entry, enforced_max_in_flight: 16 }, 16, "local", 1)).toEqual({
      value: "3 of 16",
      note: null,
      fallback: false,
    });
  });

  const hidden = (name: string) => host.querySelector<HTMLInputElement>(`input[name="${name}"]`)!.value;
  const option = (label: string) =>
    [...host.querySelectorAll<HTMLButtonElement>('[role="option"]')].find((b) => b.textContent?.startsWith(label));

  it("confirms the model's default Service when one matches, with no template text", async () => {
    await renderPanel(model({ capacity_mode: "discovered", capacity_source: "kubernetes" }), view());
    const chosen = host.querySelector('[data-testid="service-chosen"]')!;
    expect(chosen.textContent).toContain("Using up in inference · 2 ready");
    expect(chosen.textContent).toContain("(default)");
    expect(host.querySelector('[role="listbox"]')).toBeNull();
    expect(hidden("capacity_service")).toBe("");
    expect(hidden("capacity_namespace")).toBe("");
    expect(host.textContent).not.toContain("{");
    expect(host.textContent).toContain("pick a Service that selects only the pods that take requests");
  });

  it("picks a Service and namespace together from the searchable list", async () => {
    await renderPanel(model({ capacity_mode: "discovered", capacity_source: "kubernetes" }), view());
    const change = [...host.querySelectorAll("button")].find((b) => b.textContent === "Change")!;
    await act(async () => change.click());
    const labels = [...host.querySelectorAll('[role="option"]')].map((o) => o.textContent);
    expect(labels).toEqual(["Use default · up", "up · inference · 2 ready", "up-head · inference · 1 ready", "other · batch · 0 ready"]);
    const search = host.querySelector<HTMLInputElement>('input[aria-label="Search Services"]')!;
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
      setter.call(search, "batch");
      search.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect([...host.querySelectorAll('[role="option"]')].map((o) => o.textContent)).toEqual([
      "Use default · up",
      "other · batch · 0 ready",
    ]);
    await act(async () => option("other")!.click());
    expect(hidden("capacity_service")).toBe("other");
    expect(hidden("capacity_namespace")).toBe("batch");
    expect(host.querySelector('[data-testid="service-chosen"]')!.textContent).toContain("Using other in batch · 0 ready");
    expect(host.textContent).not.toContain("{");
  });

  it("clears the Service and namespace with Use default", async () => {
    await renderPanel(
      model({ capacity_mode: "discovered", capacity_source: "kubernetes", capacity_service: "up-head", capacity_namespace: "inference" }),
      view(),
    );
    expect(hidden("capacity_service")).toBe("up-head");
    const change = [...host.querySelectorAll("button")].find((b) => b.textContent === "Change")!;
    await act(async () => change.click());
    await act(async () => option("Use default")!.click());
    expect(hidden("capacity_service")).toBe("");
    expect(hidden("capacity_namespace")).toBe("");
    expect(host.querySelector('[data-testid="service-chosen"]')!.textContent).toContain("(default)");
  });

  it("opens the list when no Service matches the default", async () => {
    await renderPanel(
      model({ capacity_mode: "discovered", capacity_source: "kubernetes" }),
      view(),
      services({ default_match: null, default_service: "up" }),
    );
    expect(host.querySelector('[role="listbox"]')).not.toBeNull();
    expect(host.textContent).toContain("No Service named up was found in the allowed namespaces");
  });

  it("says why the Service list is empty", async () => {
    await renderPanel(
      model({ capacity_mode: "discovered", capacity_source: "kubernetes" }),
      view(),
      services({ services: [], default_match: null, default_service: null, reason: "no namespaces are configured for the kubernetes source (OBLETH_CAPACITY_DISCOVERY_NAMESPACES)" }),
    );
    expect(host.textContent).toContain("no namespaces are configured for the kubernetes source");
    expect(host.textContent).not.toContain("{");
  });

  it("marks per-replica concurrency required for the kubernetes source, with a hint", async () => {
    await renderPanel(model({ capacity_mode: "discovered", capacity_source: "kubernetes" }), view());
    const perReplica = host.querySelector<HTMLInputElement>('input[name="per_replica_max_in_flight"]')!;
    expect(perReplica.required).toBe(true);
    const text = host.textContent ?? "";
    expect(text).toContain("Per-replica concurrency (required)");
    expect(text).toContain("vLLM --max-num-seqs");
  });

  it("hides the kubernetes fields for the endpoints source", async () => {
    await renderPanel(model({ capacity_mode: "discovered", capacity_source: "endpoints" }), view());
    expect(host.querySelector('input[name="capacity_service"]')).toBeNull();
    expect(host.querySelector('input[name="capacity_namespace"]')).toBeNull();
    const perReplica = host.querySelector<HTMLInputElement>('input[name="per_replica_max_in_flight"]');
    expect(perReplica).not.toBeNull();
    expect(perReplica!.required).toBe(false);
  });

  it("saves the form through the discovery action", async () => {
    await renderPanel(model({ capacity_mode: "discovered", capacity_source: "endpoints" }), view());
    const form = host.querySelector("form")!;
    host.querySelector<HTMLInputElement>('input[name="per_replica_max_in_flight"]')!.value = "4";
    await act(async () => {
      form.requestSubmit();
    });
    expect(setModelCapacityDiscoveryAction).toHaveBeenCalled();
    const [id, data] = vi.mocked(setModelCapacityDiscoveryAction).mock.calls[0];
    expect(id).toBe("u");
    expect((data as FormData).get("capacity_source")).toBe("endpoints");
    expect((data as FormData).get("per_replica_max_in_flight")).toBe("4");
  });

  it("says why a model is not discovered", async () => {
    await act(async () => {
      root.render(
        <CapacityDiscoveryStatus
          status={{
            ...view().models[0].status,
            state: "stale",
            ready_replicas: 0,
            reason: "Service up in inference has no ready endpoint (1 listed); keeping the last discovered value",
          }}
          entry={{ enforced_max_in_flight: 16, cluster_in_flight: null, in_flight: 0 }}
          mode="local"
          replicas={1}
        />,
      );
    });
    expect(host.textContent).toContain("stale");
    expect(host.textContent).toContain("keeping the last discovered value");
    expect(host.textContent).not.toContain("gateways");
  });
});
