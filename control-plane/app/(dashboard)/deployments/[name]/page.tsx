import { notFound, redirect } from "next/navigation";
import { ManagedPage } from "@/components/deployments/managed-page";
import { WatchedPage } from "@/components/deployments/watched-page";
import { requireAdmin } from "@/lib/auth/roles";
import { loadDeployment } from "@/lib/deployments-data";
import { deploymentHref } from "@/lib/deployments-model";
import { modelHref } from "@/lib/models-model";
import { obleth, type AuditEntry } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

const READ_FAILED = "obleth couldn't read the models or their launch settings just now. Try again in a moment.";

export default async function DeploymentPage({ params }: { params: Promise<{ name: string }> }) {
  await requireAdmin();
  const name = decodeURIComponent((await params).name);
  // Uncached: right after a launch the redirect can land on a replica whose
  // cached list doesn't have the new model yet, and the page would 404.
  const models = await obleth.listModels().catch(() => null);
  // A failed read isn't a missing deployment: say so rather than 404 or send the page to Models.
  if (!models) throw new Error(READ_FAILED);
  const model = models.find((m) => m.model_name === name) ?? models.find((m) => m.aliases?.includes(name));
  if (!model) notFound();
  if (model.model_name !== name) redirect(deploymentHref(model.model_name));
  // This model's changes under both entity types, from the audit log's entity index.
  const [data, ...audit] = await Promise.all([
    loadDeployment(model),
    ...["model", "managed_model"].map((entityType) => safe(obleth.auditQuery({ entityType, entityId: model.id, limit: 12 }), [] as AuditEntry[])),
  ] as const);
  if (data.failed?.includes("specs")) throw new Error(READ_FAILED);
  const spec = data.spec;
  // A model obleth neither launches nor watches has no deployment: its page is in Models.
  if (!spec && model.capacity_source !== "kubernetes") redirect(modelHref(model.model_name));
  const changes = (audit as AuditEntry[][]).flat().sort((a, b) => b.id - a.id).slice(0, 12);
  return spec ? <ManagedPage key={model.id} modelId={model.id} initial={data} changes={changes} /> : <WatchedPage key={model.id} modelId={model.id} initial={data} changes={changes} />;
}
