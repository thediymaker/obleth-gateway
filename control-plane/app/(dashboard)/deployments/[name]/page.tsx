import { notFound, redirect } from "next/navigation";
import { ManagedPage } from "@/components/deployments/managed-page";
import { WatchedPage } from "@/components/deployments/watched-page";
import { requireAdmin } from "@/lib/auth/roles";
import { loadDeployments } from "@/lib/deployments-data";
import { deploymentHref } from "@/lib/deployments-model";
import { modelHref } from "@/lib/models-model";
import { obleth, type AuditEntry } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function DeploymentPage({ params }: { params: Promise<{ name: string }> }) {
  await requireAdmin();
  const name = decodeURIComponent((await params).name);
  const [data, audit] = await Promise.all([loadDeployments(), safe(obleth.audit(500), [] as AuditEntry[])]);
  const model = data.models.find((m) => m.model_name === name) ?? data.models.find((m) => m.aliases?.includes(name));
  if (!model) notFound();
  if (model.model_name !== name) redirect(deploymentHref(model.model_name));
  const spec = data.specs.find((s) => s.model_id === model.id);
  // A model obleth neither launches nor watches has no deployment: its page is in Models.
  if (!spec && model.capacity_source !== "kubernetes") redirect(modelHref(model.model_name));
  const changes = audit
    .filter((e) => (e.entity_type === "model" || e.entity_type === "managed_model") && e.entity_id === model.id)
    .slice(0, 12);
  return spec ? <ManagedPage key={model.id} modelId={model.id} initial={data} changes={changes} /> : <WatchedPage key={model.id} modelId={model.id} initial={data} changes={changes} />;
}
