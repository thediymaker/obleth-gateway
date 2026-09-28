import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { saveModelSettingsAction } from "@/app/actions";
import { ModelSettings } from "./model-settings";
import type { ModelHealthSummary, ModelRoute } from "@/lib/obleth";

vi.mock("@/app/actions", () => ({
  saveModelSettingsAction: vi.fn(async () => ({ ok: true })),
  applyAutotuneCapacityAction: vi.fn(),
  autotuneModelAction: vi.fn(),
}));
const refresh = vi.fn();
vi.mock("next/navigation", () => ({ useRouter: () => ({ refresh, push: vi.fn() }) }));

const model: ModelRoute = {
  id: "u", model_name: "m", description: "old words", upstream_model: "up", api_base: "http://a",
  api_key_set: true, model_type: "chat", quantization: "unknown", aliases: [],
  input_cost_per_token: 0.0000006, output_cost_per_token: 0.0000025,
  cost_per_image: 0, cost_per_audio_second: 0, cost_per_character: 0, cost_per_video: 0, energy_slots_per_node: 0,
  route_bias: 1, auto_eligible: true, draft_model: "", verify_api_base: "", verify_upstream_model: "",
  context_window: 8192, admission_weight: 100, max_in_flight: 16, capacity_mode: "static",
  capacity_tuned_at: null, supports_function_calling: true, supports_system_messages: true,
  supports_response_schema: false, supports_tool_choice: true, supports_vision: false,
  enabled: true, cache_enabled: false, cache_ttl_secs: 300, request_timeout_secs: null,
  max_retries: 1, retry_backoff_ms: 200, endpoint_selection_mode: "failover", debug_diagnostics: false,
  tags: ["coding:3"], boons: [], tool_servers: [], created_at: "", updated_at: "",
};

const summary: ModelHealthSummary = {
  model_id: "u", model_name: "m", checks_enabled: true, alerts_enabled: true, check_interval_secs: 900,
  failure_threshold: 2, maintenance_until: null, maintenance_note: null, status: "healthy", consecutive_failures: 0,
  alert_state: "ok", next_check_at: "", last_checked_at: null, last_latency_ms: null, last_http_status: null,
  last_message: null, updated_at: "",
};

let root: Root;
let host: HTMLDivElement;
const frame = () => act(async () => { await new Promise((r) => requestAnimationFrame(() => r(null))); });

async function render() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, enabled: false } } });
  await act(async () => {
    root.render(
      <QueryClientProvider client={client}>
        <ModelSettings model={model} summary={summary} checks={[]} mcpServers={[]} modelNames={["m"]} boonBlockers={{}} load={{ inFlight: 3, cap: 16, queued: 0 }} onStateChange={() => {}} />
      </QueryClientProvider>,
    );
  });
  await frame();
}

function type(name: string, value: string) {
  const el = host.querySelector<HTMLInputElement>(`[name="${name}"]`)!;
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
  setter.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

const bar = () => host.querySelector('[aria-label="Unsaved changes"]');
const button = (label: string) => [...host.querySelectorAll("button")].find((b) => b.textContent === label)!;

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  vi.mocked(saveModelSettingsAction).mockClear();
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

describe("the model settings form", () => {
  it("opens with nothing to save", async () => {
    await render();
    expect(bar()).toBeNull();
    expect(host.querySelector("[data-changed]")).toBeNull();
  });

  it("marks an edited setting, says what it was, and lists it in the save bar", async () => {
    await render();
    await act(async () => type("description", "new words"));
    await frame();
    expect(bar()!.textContent).toContain("1 unsaved");
    expect(bar()!.textContent).toContain("Description");
    const row = host.querySelector("#set-description")!;
    expect(row.hasAttribute("data-changed")).toBe(true);
    expect(row.textContent).toContain("was old words");
  });

  it("forgets an edit that is put back", async () => {
    await render();
    await act(async () => type("description", "new words"));
    await frame();
    await act(async () => type("description", "old words"));
    await frame();
    expect(bar()).toBeNull();
  });

  it("counts a switch turned off, which submits nothing", async () => {
    await render();
    await act(async () => host.querySelector<HTMLInputElement>('[name="auto_eligible"]')!.click());
    await frame();
    expect(bar()!.textContent).toContain("Eligible for auto");
  });

  it("saves every change at once, naming the sections it touched", async () => {
    await render();
    await act(async () => type("description", "new words"));
    await act(async () => type("max_retries", "3"));
    await act(async () => type("max_in_flight", "32"));
    await frame();
    expect(bar()!.textContent).toContain("3 unsaved");
    await act(async () => button("Save changes").click());
    await frame();
    const data = vi.mocked(saveModelSettingsAction).mock.calls[0][0] as FormData;
    expect(data.get("id")).toBe("u");
    expect(String(data.get("sections")).split(",").sort()).toEqual(["capacity", "model", "reliability"]);
    expect(data.get("description")).toBe("new words");
    expect(data.get("has_tags")).toBe("1");
    // Untouched prices go back exactly as stored, per token.
    expect(data.get("input_cost_per_token")).toBe("0.0000006");
    expect(bar()!.textContent).toContain("Saved.");
    expect(bar()!.textContent).not.toContain("unsaved");
    expect(refresh).toHaveBeenCalled();
  });

  it("puts everything back on Discard", async () => {
    await render();
    await act(async () => type("description", "new words"));
    await frame();
    await act(async () => button("Discard").click());
    await frame();
    expect(bar()).toBeNull();
    expect(host.querySelector<HTMLInputElement>('[name="description"]')!.value).toBe("old words");
  });

  it("keeps the failed part marked when a save is refused", async () => {
    vi.mocked(saveModelSettingsAction).mockResolvedValueOnce({ ok: false, error: "Delivery: max_retries must be at most 5", saved: ["model"] });
    await render();
    await act(async () => type("description", "new words"));
    await act(async () => type("max_retries", "9"));
    await frame();
    await act(async () => button("Save changes").click());
    await frame();
    expect(bar()!.textContent).toContain("max_retries must be at most 5");
    expect(bar()!.textContent).toContain("1 unsaved");
    expect(bar()!.textContent).toContain("Retries");
    expect(bar()!.textContent).not.toContain("Description");
  });

  it("shows a price per million and submits it per token", async () => {
    await render();
    const price = host.querySelector<HTMLInputElement>('[aria-label="Input, per 1M tokens"]')!;
    expect(price.value).toBe("0.6");
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
    await act(async () => { setter.call(price, "1.2"); price.dispatchEvent(new Event("input", { bubbles: true })); });
    await frame();
    expect(host.querySelector<HTMLInputElement>('input[type="hidden"][name="input_cost_per_token"]')!.value).toBe("0.0000012");
    expect(bar()!.textContent).toContain("Price");
  });
});
