// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { FairshareLiveView, ModelHealthSummary, ModelRoute } from "@/lib/obleth";

// A poll tick must re-render only the cells that show the live load. These
// count renders of a component inside each row (ProviderMark) and inside every
// settings form (TextField); both are plain functions in these mocks, so they
// run whenever their parent renders.
const counts = vi.hoisted(() => ({ provider: 0, field: 0 }));
vi.mock("@/components/models/ui", async (importOriginal) => {
  const real = await importOriginal<typeof import("@/components/models/ui")>();
  return { ...real, ProviderMark: (props: Parameters<typeof real.ProviderMark>[0]) => { counts.provider += 1; return real.ProviderMark(props); } };
});
vi.mock("@/components/models/fields", async (importOriginal) => {
  const real = await importOriginal<typeof import("@/components/models/fields")>();
  return { ...real, TextField: (props: Parameters<typeof real.TextField>[0]) => { counts.field += 1; return real.TextField(props); } };
});
vi.mock("@/app/actions", () => {
  const ok = () => vi.fn(async () => ({ ok: true }));
  return Object.fromEntries(
    [
      "activateModelAction", "applyAutotuneCapacityAction", "applyModelManifestAction", "autotuneModelAction", "checkModelHealthAction", "checkModelsHealthAction",
      "createModelAction", "createModelEndpointAction", "deleteModelAction", "deleteModelEndpointAction", "deleteModelFamilyAction", "deleteModelsAction",
      "listUpstreamModelsAction", "restartReplicaAction", "saveModelFamilyAction", "saveModelSettingsAction", "setModelEnabledAction", "setModelReasoningAction",
      "setModelsEnabledAction", "setModelStatusAction", "updateModelEndpointAction",
    ].map((name) => [name, ok()]),
  );
});
const router = { refresh: vi.fn(), push: vi.fn(), replace: vi.fn() };
vi.mock("next/navigation", () => ({ useRouter: () => router, usePathname: () => "/models", useSearchParams: () => new URLSearchParams() }));

const { ModelsList } = await import("./models-list");
const { ModelPage } = await import("./model-page");

const model = (over: Partial<ModelRoute>): ModelRoute => ({
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
  tags: [], boons: [], tool_servers: [], created_at: "", updated_at: "",
  ...over,
}) as ModelRoute;
const summary = (id: string): ModelHealthSummary => ({
  model_id: id, model_name: id, checks_enabled: true, alerts_enabled: true, check_interval_secs: 900,
  failure_threshold: 2, maintenance_until: null, maintenance_note: null, status: "healthy", consecutive_failures: 0,
  alert_state: "ok", next_check_at: "", last_checked_at: null, last_latency_ms: null, last_http_status: null,
  last_message: null, updated_at: "",
});
const view = (inFlight: Record<string, number>): FairshareLiveView => ({ model_in_flight: inFlight, model_queued: {}, pools: [], tenants: [], groups: [] }) as unknown as FairshareLiveView;

let root: Root;
let host: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true, IntersectionObserver: class { observe() {} unobserve() {} disconnect() {} } });
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  client = new QueryClient({ defaultOptions: { queries: { retry: false, enabled: false } } });
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

async function render(node: React.ReactNode) {
  await act(async () => root.render(<QueryClientProvider client={client}>{node}</QueryClientProvider>));
}
async function tick(v: FairshareLiveView) {
  await act(async () => {
    client.setQueryData(["fairshare-live"], v);
    await new Promise((r) => setTimeout(r, 0));
  });
}

describe("a fair-share poll on the Models list", () => {
  it("re-renders only the load cells, not the rows", async () => {
    const models = [model({ id: "a", model_name: "alpha" }), model({ id: "b", model_name: "beta" })];
    await render(<ModelsList models={models} health={[summary("a"), summary("b")]} managed={{}} slurmEnabled={false} />);
    await tick(view({ alpha: 1, beta: 0 }));
    const row = (name: string) => [...host.querySelectorAll("tr")].find((r) => r.textContent?.includes(name))!;
    expect(row("alpha").textContent).toContain("1/16");
    const before = counts.provider;
    await tick(view({ alpha: 5, beta: 0 }));
    expect(row("alpha").textContent).toContain("5/16");
    expect(row("beta").textContent).toContain("0/16");
    expect(counts.provider - before).toBe(0);
  });
});

describe("a fair-share poll on a model's page", () => {
  const props = { summary: summary("u"), managed: false, mcpServers: [], modelNames: ["m"], boonBlockers: {} };

  it("updates the load without re-rendering the settings forms", async () => {
    const m = model({});
    await render(<ModelPage model={m} {...props} />);
    await tick(view({ m: 2 }));
    const capacity = () => host.querySelector("#capacity")!.textContent;
    expect(capacity()).toContain("2 of 16 slots in use right now.");
    const before = counts.field;
    await tick(view({ m: 7 }));
    expect(capacity()).toContain("7 of 16 slots in use right now.");
    expect(counts.field - before).toBe(0);
  });

  it("keeps an unsaved edit across a poll", async () => {
    await render(<ModelPage model={model({})} {...props} />);
    const input = host.querySelector<HTMLInputElement>('[name="description"]')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, "new words");
      input.dispatchEvent(new Event("input", { bubbles: true }));
      await new Promise((r) => requestAnimationFrame(() => r(null)));
    });
    await tick(view({ m: 3 }));
    expect(host.querySelector<HTMLInputElement>('[name="description"]')!.value).toBe("new words");
    expect(host.querySelector('[aria-label="Unsaved changes"]')).not.toBeNull();
  });
});
