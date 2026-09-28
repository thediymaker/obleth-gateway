import { describe, expect, it } from "vitest";
import type { ModelRoute } from "@/lib/obleth";
import { toPortalModelSummary } from "./portal-model-summary";

const fullRoute = {
  id: "m-1",
  model_name: "chat-large",
  aliases: ["chat-old"],
  description: "General chat model",
  upstream_model: "vendor/chat-large-instruct",
  api_base: "http://10.0.0.5:8000/v1",
  api_key: "sk-upstream-secret",
  model_type: "chat",
  quantization: "fp8",
  verify_api_base: "http://10.0.0.6:8000/v1",
  verify_upstream_model: "vendor/verifier",
  context_window: 32768,
  supports_function_calling: true,
  supports_system_messages: true,
  supports_response_schema: false,
  supports_tool_choice: true,
  supports_vision: false,
  enabled: true,
  tags: ["code:2"],
  boons: ["vision"],
  endpoints: [{ api_base: "http://10.0.0.7:8000/v1", api_key: "sk-endpoint-secret" }],
} as unknown as ModelRoute;

describe("toPortalModelSummary", () => {
  it("drops upstream URLs, secrets, and endpoint topology", () => {
    const summary = toPortalModelSummary(fullRoute);
    for (const key of ["api_key", "api_base", "verify_api_base", "upstream_model", "verify_upstream_model", "endpoints"]) {
      expect(summary).not.toHaveProperty(key);
    }
    const serialized = JSON.stringify(summary);
    expect(serialized).not.toContain("secret");
    expect(serialized).not.toContain("10.0.0.");
  });

  it("keeps exactly the user-facing fields the portal renders", () => {
    expect(toPortalModelSummary(fullRoute)).toEqual({
      id: "m-1",
      model_name: "chat-large",
      aliases: ["chat-old"],
      description: "General chat model",
      model_type: "chat",
      quantization: "fp8",
      tags: ["code:2"],
      boons: ["vision"],
      context_window: 32768,
      supports_function_calling: true,
      supports_system_messages: true,
      supports_response_schema: false,
      supports_tool_choice: true,
      supports_vision: false,
      enabled: true,
    });
  });
});
