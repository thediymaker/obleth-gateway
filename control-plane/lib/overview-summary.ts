import { obleth, type CostAgg, type ModelRoute, type Tenant, type UsageAgg, type ApiKey } from "@/lib/obleth";

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

export function computeOverviewSummary(
  tenants: Tenant[],
  keys: ApiKey[],
  models: ModelRoute[],
  usage: UsageAgg[],
  costs: CostAgg[],
): OverviewSummary {
  const totalRequests = usage.reduce((a, u) => a + Number(u.requests), 0);
  const totalTokens = usage.reduce((a, u) => a + Number(u.total_tokens), 0);
  const totalCost = costs.reduce((a, c) => a + c.total_cost, 0);
  const hasPricing = models.some((m) => m.input_cost_per_token > 0 || m.output_cost_per_token > 0);
  const activeTenants = usage.filter((u) => Number(u.requests) > 0).length;
  const enabledModels = models.filter((m) => m.enabled).length;

  return {
    requests: totalRequests,
    tokens: totalTokens,
    cost: totalCost,
    hasPricing,
    tenantCount: tenants.length,
    activeTenants,
    modelCount: models.length,
    enabledModels,
    keyCount: keys.length,
  };
}

export async function fetchOverviewSummary(): Promise<OverviewSummary> {
  const [tenants, keys, models, usage, costs] = await Promise.all([
    obleth.listTenants(),
    obleth.listKeys(),
    obleth.listModels(),
    obleth.usage(),
    obleth.costs(),
  ]);
  return computeOverviewSummary(tenants, keys, models, usage, costs);
}
