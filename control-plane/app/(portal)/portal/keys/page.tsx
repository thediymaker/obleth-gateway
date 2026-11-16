import { currentPortalContext } from "@/lib/portal-context";
import { accounts } from "@/lib/obleth-accounts";
import { obleth, type ApiKey, type KeyUsageSummary, type ModelRoute, type Tenant } from "@/lib/obleth";
import { safe } from "@/lib/safe";
import { PortalKeys } from "@/components/portal/portal-keys";

export const dynamic = "force-dynamic";

export default async function PortalKeysPage() {
  const ctx = await currentPortalContext();
  const tenantId = ctx.current.id;
  const sinceMs = Date.now() - 24 * 60 * 60_000;
  const [keys, keyUsage, models, tenants] = await Promise.all([
    safe(
      accounts.listOwnedKeys(tenantId, ctx.current.role === "manager" ? undefined : ctx.identity),
      [] as ApiKey[],
    ),
    safe(obleth.usageKeysSummary({ tenantId, sinceMs, limit: 500 }), [] as KeyUsageSummary[]),
    safe(obleth.listModels(), [] as ModelRoute[]),
    safe(obleth.listTenants(), [] as Tenant[]),
  ]);
  const tenant = tenants.find((row) => row.id === tenantId) ?? null;
  const allowed = tenant?.allowed_models?.filter(Boolean) ?? [];
  const defaultModel =
    models
      .filter((model) => model.enabled)
      .filter((model) => allowed.length === 0 || allowed.includes(model.model_name))
      .sort((a, b) => a.model_name.localeCompare(b.model_name))[0]?.model_name ?? "model-name";

  return (
    <PortalKeys
      keys={keys}
      keyUsage={keyUsage}
      gatewayBase={process.env.OBLETH_PROXY_BASE_URL ?? "http://localhost:8080"}
      defaultModel={defaultModel}
    />
  );
}
