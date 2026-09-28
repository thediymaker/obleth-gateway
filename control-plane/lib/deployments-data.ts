import type { CapacityDiscoveryView, ManagedModelSpec, ModelReplica, ModelRoute, SlurmSettingsView } from "@/lib/obleth";
import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export interface DeploymentsData {
  now: number;
  models: ModelRoute[];
  specs: ManagedModelSpec[];
  replicas: ModelReplica[];
  discovery: CapacityDiscoveryView | null;
  /** Requests in the last 24 hours, by model name. */
  requests: Record<string, number>;
  slurm: SlurmSettingsView | null;
}

/** Everything the Deployments list and a deployment's page read, in one go. */
export async function loadDeployments(now = Date.now()): Promise<DeploymentsData> {
  const [models, specs, replicas, discovery, usage, slurm] = await Promise.all([
    safe(obleth.listModels(), [] as ModelRoute[]),
    safe(obleth.listManagedModels(), [] as ManagedModelSpec[]),
    safe(obleth.listAllReplicas(), [] as ModelReplica[]),
    safe<CapacityDiscoveryView | null>(obleth.capacityDiscovery(), null),
    safe(obleth.usageByModel(now - 86_400_000), []),
    safe<SlurmSettingsView | null>(obleth.getSlurmSettings(), null),
  ]);
  return {
    now,
    models,
    specs,
    replicas,
    discovery,
    requests: Object.fromEntries(usage.map((u) => [u.model, Number(u.requests)])),
    slurm,
  };
}
