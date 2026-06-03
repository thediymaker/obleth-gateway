import { KeyManager } from "@/components/key-manager";
import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function KeysPage() {
  const [tenants, keys, usageByKey] = await Promise.all([
    safe(obleth.listTenants(), []),
    safe(obleth.listKeys(), []),
    safe(obleth.usageByKey(), []),
  ]);

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">API Keys</h1>
        <p className="text-sm text-muted-foreground">Tenant credentials and per-key usage</p>
      </div>
      <KeyManager tenants={tenants} keys={keys} usageByKey={usageByKey} />
    </div>
  );
}
