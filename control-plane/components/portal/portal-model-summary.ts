import type { ModelRoute } from "@/lib/obleth";

/**
 * The only model fields a tenant portal user may see. Built server-side by an
 * explicit allowlist so upstream URLs, upstream secrets, and endpoint topology
 * never reach the browser, even when `ModelRoute` grows new fields.
 */
export interface PortalModelSummary {
  id: string;
  model_name: string;
  /** Client-facing alternate names; callers may already have one hard-coded. */
  aliases: string[];
  description: string;
  model_type: string;
  quantization: string;
  tags: string[];
  /** Gateway-side capability grants (names only, no helper configuration). */
  boons: string[];
  context_window: number;
  supports_function_calling: boolean;
  supports_system_messages: boolean;
  supports_response_schema: boolean;
  supports_tool_choice: boolean;
  supports_vision: boolean;
  enabled: boolean;
}

export function toPortalModelSummary(model: ModelRoute): PortalModelSummary {
  return {
    id: model.id,
    model_name: model.model_name,
    aliases: model.aliases ?? [],
    description: model.description,
    model_type: model.model_type,
    quantization: model.quantization,
    tags: model.tags ?? [],
    boons: model.boons ?? [],
    context_window: model.context_window,
    supports_function_calling: model.supports_function_calling,
    supports_system_messages: model.supports_system_messages,
    supports_response_schema: model.supports_response_schema,
    supports_tool_choice: model.supports_tool_choice,
    supports_vision: model.supports_vision,
    enabled: model.enabled,
  };
}
