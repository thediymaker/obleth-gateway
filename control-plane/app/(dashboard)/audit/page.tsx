import { AuditTable } from "@/components/audit-table";
import { obleth, type AuditEntry } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function AuditPage() {
  const entries = await safe(obleth.audit(1000), [] as AuditEntry[]);

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Audit log</h1>
        <p className="text-sm text-muted-foreground">Configuration changes recorded from the Management API</p>
      </div>

      <AuditTable entries={entries} />
    </div>
  );
}
