import { createHash } from "node:crypto";
import type { ModelRoute, ModelEndpoint } from "@/lib/obleth";

export function configFingerprint(model: ModelRoute, endpoints: ModelEndpoint[]): string {
  const canonical = {
    model_name: model.model_name,
    upstream_model: model.upstream_model,
    api_base: model.api_base,
    endpoint_selection_mode: model.endpoint_selection_mode,
    max_in_flight: model.max_in_flight,
    admission_weight: model.admission_weight,
    boons: [...model.boons].sort(),
    tool_servers: [...model.tool_servers].sort(),
    endpoints: [...endpoints]
      .map((e) => ({ name: e.name, api_base: e.api_base, priority: e.priority, weight: e.weight, enabled: e.enabled }))
      .sort((a, b) => a.name.localeCompare(b.name)),
  };
  return createHash("sha256").update(JSON.stringify(canonical)).digest("hex").slice(0, 16);
}
