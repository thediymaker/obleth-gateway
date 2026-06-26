import { PortalModels } from "@/components/portal/portal-models";
import { requireUser } from "@/lib/auth/roles";
import { obleth, type ModelRoute, type Tenant } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function PortalModelsPage() {
  const session = await requireUser();
  const [models, tenants] = await Promise.all([
    safe(obleth.listModels(), [] as ModelRoute[]),
    safe(obleth.listTenants(), [] as Tenant[]),
  ]);
  const tenant = tenants.find((row) => row.id === session.tenantId) ?? null;
  const allowed = tenant?.allowed_models?.filter(Boolean) ?? [];
  const visible = models
    .filter((model) => model.enabled)
    .filter((model) => allowed.length === 0 || allowed.includes(model.model_name))
    .sort((a, b) => a.model_name.localeCompare(b.model_name));

  return (
    <PortalModels
      models={visible}
      tenant={tenant}
      gatewayBase={process.env.OBLETH_PROXY_BASE_URL ?? "http://localhost:8080"}
    />
  );
}
