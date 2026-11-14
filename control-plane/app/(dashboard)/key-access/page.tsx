import { requireAdmin } from "@/lib/auth/roles";
import { KeyAccessView } from "@/components/access/key-access-view";
import { isKeyAccessRange, keyAccessRange, type KeyAccessRange } from "@/lib/key-access-model";
import { obleth, type KeyRejectionAgg, type KeySource, type Tenant } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function KeyAccessPage({ searchParams }: { searchParams: Promise<{ range?: string; tenant?: string; q?: string }> }) {
  await requireAdmin();
  const q = await searchParams;
  const range: KeyAccessRange = isKeyAccessRange(q.range) ? q.range : "24h";
  const tenantId = q.tenant || undefined;
  const sinceMs = Date.now() - keyAccessRange(range).ms;
  const [tenants, sources, rejections] = await Promise.all([
    safe(obleth.listTenants(), [] as Tenant[]),
    safe(obleth.keySources({ sinceMs, tenantId }).then((r) => r as KeySource[] | null), null),
    safe(obleth.keyRejections({ sinceMs, tenantId }).then((r) => r as KeyRejectionAgg[] | null), null),
  ]);
  return (
    <KeyAccessView
      key={`${range}:${tenantId ?? ""}`}
      tenants={[...tenants].sort((a, b) => a.name.localeCompare(b.name)).map((t) => ({ id: t.id, name: t.name }))}
      initialRange={range}
      initialTenant={tenantId ?? ""}
      initialSearch={q.q ?? ""}
      initial={sources && rejections ? { sinceMs, sources, rejections } : null}
    />
  );
}
