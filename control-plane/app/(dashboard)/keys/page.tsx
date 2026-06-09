import { KeyManager } from "@/components/key-manager";
import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

// Window for the per-key usage summary (requests/tokens/cost and "last used").
// 30 days keeps "last used" meaningful for keys that aren't hit daily while
// still bounding the ClickHouse scan.
const KEY_USAGE_WINDOW_MS = 30 * 24 * 60 * 60 * 1000;

export default async function KeysPage() {
  const [tenants, keys, keyUsage] = await Promise.all([
    safe(obleth.listTenants(), []),
    safe(obleth.listKeys(), []),
    safe(obleth.usageKeysSummary({ sinceMs: Date.now() - KEY_USAGE_WINDOW_MS, limit: 5000 }), []),
  ]);

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">API Keys</h1>
        <p className="text-sm text-muted-foreground">Tenant credentials and per-key usage (last 30 days)</p>
      </div>
      <KeyManager tenants={tenants} keys={keys} keyUsage={keyUsage} />
    </div>
  );
}
