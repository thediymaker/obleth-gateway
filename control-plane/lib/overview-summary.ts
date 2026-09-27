import { obleth, type OverviewSummaryView } from "@/lib/obleth";

export interface OverviewSummary {
  requests: number;
  tokens: number;
  cost: number;
  hasPricing: boolean;
  tenantCount: number;
  activeTenants: number;
  modelCount: number;
  enabledModels: number;
  keyCount: number;
  inputTokens: number;
  outputTokens: number;
  errors: number;
  p50TtftMs: number;
  avgTtftMs: number;
  energyWh: number;
  energyCostUsd: number;
  co2G: number;
  /** Whether the gateway reported the window detail (errors, latency, energy). Older gateways do not. */
  detailed: boolean;
}

export const EMPTY_OVERVIEW_SUMMARY: OverviewSummary = {
  requests: 0,
  tokens: 0,
  cost: 0,
  hasPricing: false,
  tenantCount: 0,
  activeTenants: 0,
  modelCount: 0,
  enabledModels: 0,
  keyCount: 0,
  inputTokens: 0,
  outputTokens: 0,
  errors: 0,
  p50TtftMs: 0,
  avgTtftMs: 0,
  energyWh: 0,
  energyCostUsd: 0,
  co2G: 0,
  detailed: false,
};

export function toOverviewSummary(view: OverviewSummaryView): OverviewSummary {
  return {
    requests: Number(view.requests),
    tokens: Number(view.tokens),
    cost: Number(view.cost),
    hasPricing: view.has_pricing,
    tenantCount: view.tenant_count,
    activeTenants: Number(view.active_tenants),
    modelCount: view.model_count,
    enabledModels: view.enabled_models,
    keyCount: view.key_count,
    // Absent from gateways older than the fields; read as zero.
    inputTokens: Number(view.input_tokens ?? 0),
    outputTokens: Number(view.output_tokens ?? 0),
    errors: Number(view.errors ?? 0),
    p50TtftMs: Number(view.p50_ttft_ms ?? 0),
    avgTtftMs: Number(view.avg_ttft_ms ?? 0),
    energyWh: Number(view.energy_wh ?? 0),
    energyCostUsd: Number(view.energy_cost_usd ?? 0),
    co2G: Number(view.co2_g ?? 0),
    detailed: view.errors !== undefined,
  };
}

/** One admin round-trip for the overview strip and status footer. The admin
 *  API aggregates counts and usage totals server-side, so this no longer
 *  deserializes the full tenant/key/model lists per poll. `sinceMs` defaults
 *  to the gateway's own 24-hour window. */
export async function fetchOverviewSummary(sinceMs?: number): Promise<OverviewSummary> {
  return toOverviewSummary(await obleth.overviewSummary(sinceMs));
}
