import { notFound, redirect } from "next/navigation";
import { ModelPage } from "@/components/models/model-page";
import { requireAdmin } from "@/lib/auth/roles";
import { boonBlockers } from "@/lib/boon-availability";
import { modelHref } from "@/lib/models-model";
import { obleth, type BoonSettingsView, type KnowledgeSettingsView, type McpServer, type ModelHealthSummary, type ModelRoute } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

/** What the health summary says before a model's first check. */
function emptySummary(model: ModelRoute): ModelHealthSummary {
  const now = new Date().toISOString();
  return {
    model_id: model.id,
    model_name: model.model_name,
    checks_enabled: true,
    alerts_enabled: true,
    check_interval_secs: 900,
    failure_threshold: 2,
    maintenance_until: null,
    maintenance_note: null,
    status: "unknown",
    consecutive_failures: 0,
    alert_state: "ok",
    next_check_at: now,
    last_checked_at: null,
    last_latency_ms: null,
    last_http_status: null,
    last_message: null,
    updated_at: now,
  };
}

export default async function ModelDetailPage({ params }: { params: Promise<{ name: string }> }) {
  await requireAdmin();
  const name = decodeURIComponent((await params).name);
  const models = await safe(obleth.listModels(), []);
  const model = models.find((m) => m.model_name === name) ?? models.find((m) => m.aliases?.includes(name));
  if (!model) notFound();
  // An alias leads to the model's own name, so there is one address per model.
  if (model.model_name !== name) redirect(modelHref(model.model_name));

  const [health, mcpServers, managedSpecs, boonSettings, knowledgeSettings] = await Promise.all([
    safe<ModelHealthSummary[]>(obleth.modelHealth(), []),
    safe<McpServer[]>(obleth.listMcpServers(), []),
    safe(obleth.listManagedModels(), []),
    safe<BoonSettingsView | null>(obleth.getBoonSettings(), null),
    safe<KnowledgeSettingsView | null>(obleth.getKnowledgeSettings(), null),
  ]);

  return (
    <ModelPage
      key={model.id}
      model={model}
      summary={health.find((h) => h.model_id === model.id) ?? emptySummary(model)}
      managed={managedSpecs.some((spec) => spec.model_id === model.id)}
      mcpServers={mcpServers}
      modelNames={models.map((m) => m.model_name)}
      boonBlockers={boonBlockers(boonSettings, knowledgeSettings)}
    />
  );
}
