import { describe, it, expect } from "vitest";
import { configFingerprint } from "./fingerprint";
import type { ModelRoute, ModelEndpoint } from "@/lib/obleth";

const model = (over: Partial<ModelRoute> = {}): ModelRoute => ({
  id: "u", model_name: "m", description: "", upstream_model: "up", api_base: "http://a",
  api_key: null, model_type: "chat", input_cost_per_token: 0, output_cost_per_token: 0,
  cost_per_image: 0, cost_per_audio_second: 0, cost_per_character: 0, energy_slots_per_node: 1,
  context_window: 8192, admission_weight: 1, max_in_flight: null, capacity_mode: "static",
  capacity_tuned_at: null, supports_function_calling: true, supports_system_messages: true,
  supports_response_schema: false, supports_tool_choice: false, supports_vision: false,
  enabled: true, cache_enabled: false, cache_ttl_secs: 0, request_timeout_secs: null,
  max_retries: 0, retry_backoff_ms: 0, endpoint_selection_mode: "priority", debug_diagnostics: false,
  tags: [], boons: ["compression", "vision"], tool_servers: ["srv"], created_at: "", updated_at: "",
  ...over,
});
const ep = (over: Partial<ModelEndpoint> = {}): ModelEndpoint => ({
  id: "e", model_id: "u", name: "e1", api_base: "http://a", api_key: null, priority: 0, weight: 1,
  enabled: true, health_status: "healthy", consecutive_failures: 0, alert_state: "ok",
  last_checked_at: null, last_latency_ms: null, last_http_status: null, last_message: null,
  created_at: "", updated_at: "", ...over,
});

describe("configFingerprint", () => {
  it("is stable regardless of boon / endpoint ordering", () => {
    const a = configFingerprint(model({ boons: ["vision", "compression"] }), [ep({ name: "b" }), ep({ name: "a" })]);
    const b = configFingerprint(model({ boons: ["compression", "vision"] }), [ep({ name: "a" }), ep({ name: "b" })]);
    expect(a).toBe(b);
  });
  it("changes when a routing-relevant field changes", () => {
    const a = configFingerprint(model(), [ep()]);
    const b = configFingerprint(model(), [ep({ api_base: "http://other" })]);
    expect(a).not.toBe(b);
  });
  it("ignores health/timestamp churn", () => {
    const a = configFingerprint(model(), [ep({ health_status: "healthy", last_latency_ms: 10 })]);
    const b = configFingerprint(model(), [ep({ health_status: "unhealthy", last_latency_ms: 999 })]);
    expect(a).toBe(b);
  });
});
