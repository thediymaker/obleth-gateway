import { AuditTable } from "@/components/audit-table";
import { obleth, type AuditEntry, type Tenant } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function AuditPage() {
  const [entries, tenants] = await Promise.all([
    safe(obleth.audit(1000), [] as AuditEntry[]),
    safe(obleth.listTenants(), [] as Tenant[]),
  ]);

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Audit</h1>
        <p className="max-w-2xl text-sm text-muted-foreground">
          Configuration and control-plane changes, newest first. Filter by actor, action, or target to follow what changed across users.
        </p>
      </div>

      <AuditTable entries={entries} tenants={tenants.map((tenant) => ({ id: tenant.id, name: tenant.name }))} />
    </div>
  );
}
