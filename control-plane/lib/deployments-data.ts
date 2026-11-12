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
  /** Which reads failed this time ("slurm", "replicas", "specs", ...), so the
   *  pages say so instead of showing an off or empty state. */
  failed?: string[];
}

/** Everything the Deployments list and a deployment's page read, in one go. */
export async function loadDeployments(now = Date.now()): Promise<DeploymentsData> {
  const failed: string[] = [];
  const note = <T>(what: string, p: Promise<T>): Promise<T> => p.catch((e) => { failed.push(what); throw e; });
  const [models, specs, replicas, discovery, usage, slurm] = await Promise.all([
    // Uncached: right after a launch the redirect can land on a replica whose
    // cached list doesn't have the new model yet, and the page would 404.
    safe(note("models", obleth.listModelsFresh()), [] as ModelRoute[]),
    safe(note("specs", obleth.listManagedModels()), [] as ManagedModelSpec[]),
    safe(note("replicas", obleth.listAllReplicas()), [] as ModelReplica[]),
    safe<CapacityDiscoveryView | null>(obleth.capacityDiscovery(), null),
    safe(obleth.usageByModel(now - 86_400_000), []),
    safe<SlurmSettingsView | null>(note("slurm", obleth.getSlurmSettings()), null),
  ]);
  return {
    now,
    models,
    specs,
    replicas,
    discovery,
    requests: Object.fromEntries(usage.map((u) => [u.model, Number(u.requests)])),
    slurm,
    failed,
  };
}
