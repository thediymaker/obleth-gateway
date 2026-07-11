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
  };
}

/** One admin round-trip for the overview strip and status footer. The admin
 *  API aggregates counts and 24h usage totals server-side, so this no longer
 *  deserializes the full tenant/key/model lists per poll. */
export async function fetchOverviewSummary(): Promise<OverviewSummary> {
  return toOverviewSummary(await obleth.overviewSummary());
}
