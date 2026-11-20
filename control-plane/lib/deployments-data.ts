import type { CapacityDiscoveryView, ManagedModelSpec, ModelReplica, ModelRoute, SlurmSettingsView } from "@/lib/obleth";
import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";

// No read time in these: a poll that changed nothing keeps the same data
// (react-query shares unchanged structure), so memoised rows and forms skip it.
export interface DeploymentsData {
  models: ModelRoute[];
  specs: ManagedModelSpec[];
  replicas: ModelReplica[];
  discovery: CapacityDiscoveryView | null;
  slurm: SlurmSettingsView | null;
  /** Which reads failed this time ("slurm", "replicas", "specs", ...), so the
   *  pages say so instead of showing an off or empty state. */
  failed?: string[];
}

/** One deployment's own reads, for its page. */
export interface DeploymentData {
  model: ModelRoute;
  spec: ManagedModelSpec | null;
  replicas: ModelReplica[];
  /** Capacity discovery, read only for a model a Kubernetes cluster runs. */
  discovery: CapacityDiscoveryView | null;
  slurm: SlurmSettingsView | null;
  failed?: string[];
}

/** Requests in the last 24 hours, by model name. */
export type DeploymentRequests = Record<string, number>;

/** The Deployments list's fleet reads: models, launch settings, replicas and
 *  discovery. The 24 h request counts are a separate, slower read
 *  ({@link loadDeploymentRequests}) because they come from ClickHouse. */
export async function loadDeployments(): Promise<DeploymentsData> {
  const failed: string[] = [];
  const note = <T>(what: string, p: Promise<T>): Promise<T> => p.catch((e) => { failed.push(what); throw e; });
  const [models, specs, replicas, discovery, slurm] = await Promise.all([
    // Uncached: right after a launch the redirect can land on a replica whose
    // cached list doesn't have the new model yet, and the page would 404.
    safe(note("models", obleth.listModels()), [] as ModelRoute[]),
    safe(note("specs", obleth.listManagedModels()), [] as ManagedModelSpec[]),
    safe(note("replicas", obleth.listAllReplicas()), [] as ModelReplica[]),
    safe<CapacityDiscoveryView | null>(obleth.capacityDiscovery(), null),
    safe<SlurmSettingsView | null>(note("slurm", obleth.getSlurmSettings()), null),
  ]);
  return { models, specs, replicas, discovery, slurm, failed };
}

/** Every model's requests in the last 24 hours. */
export async function loadDeploymentRequests(now = Date.now()): Promise<DeploymentRequests> {
  const usage = await obleth.usageByModel({ sinceMs: now - 86_400_000 });
  return Object.fromEntries(usage.map((u) => [u.model, Number(u.requests)]));
}

/** One deployment's launch settings, replicas and either the Slurm settings
 *  (a model obleth launches) or capacity discovery (one a cluster runs), read
 *  through the per-model endpoints rather than the whole fleet. */
export async function loadDeployment(model: ModelRoute): Promise<DeploymentData> {
  const failed: string[] = [];
  const note = <T>(what: string, p: Promise<T>): Promise<T> => p.catch((e) => { failed.push(what); throw e; });
  const watched = model.capacity_source === "kubernetes";
  const [spec, replicas, slurm, discovery] = await Promise.all([
    safe<ManagedModelSpec | null>(note("specs", obleth.getManagedModel(model.id)), null),
    safe(note("replicas", obleth.listReplicas(model.id)), [] as ModelReplica[]),
    safe<SlurmSettingsView | null>(note("slurm", obleth.getSlurmSettings()), null),
    watched ? safe<CapacityDiscoveryView | null>(obleth.capacityDiscovery(), null) : Promise.resolve(null),
  ]);
  return { model, spec, replicas, discovery, slurm, failed };
}

/** {@link loadDeployment} by model id, for the page's live poll. */
export async function loadDeploymentById(id: string): Promise<DeploymentData> {
  return loadDeployment(await obleth.getModel(id));
}
