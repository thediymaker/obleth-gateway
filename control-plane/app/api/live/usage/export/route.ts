import { NextRequest } from "next/server";
import { obleth } from "@/lib/obleth";
import type { UsageDailyGroupBy } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";
import { buildUsageCsv, selectColumns } from "@/lib/usage-export";

function defaultStart(): string {
  return new Date(Date.now() - 7 * 86_400_000).toISOString().slice(0, 10);
}

function today(): string {
  return new Date().toISOString().slice(0, 10);
}

export async function GET(req: NextRequest) {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    const p = req.nextUrl.searchParams;
    const startDay = p.get("start_day") ?? defaultStart();
    const endDay = p.get("end_day") ?? today();
    const groupBy = (p.get("group_by") as UsageDailyGroupBy | null) ?? "day";
    const columns = selectColumns(p.get("columns"));

    // Name lookups make the export human-readable. Failure to resolve a name
    // is non-fatal — the id column still carries the identity.
    const needTenant = columns.includes("tenant_name");
    const needKey = columns.includes("key_prefix") || columns.includes("key_name");
    const [rows, tenants, keys] = await Promise.all([
      obleth.usageDaily({
        startDay,
        endDay,
        groupBy,
        tenantId: p.get("tenant_id") ?? undefined,
        keyId: p.get("key_id") ?? undefined,
        model: p.get("model") ?? undefined,
      }),
      needTenant ? obleth.listTenants().catch(() => []) : Promise.resolve([]),
      needKey ? obleth.listKeys().catch(() => []) : Promise.resolve([]),
    ]);

    const csv = buildUsageCsv(rows, columns, {
      startDay,
      endDay,
      tenantNames: new Map(tenants.map((t) => [t.id, t.name])),
      keyNames: new Map(keys.map((k) => [k.id, k.name])),
      keyPrefixes: new Map(keys.map((k) => [k.id, k.key_prefix])),
    });
    const filename = `usage_${startDay}_to_${endDay}.csv`;

    return new Response(csv, {
      status: 200,
      headers: {
        "Content-Type": "text/csv; charset=utf-8",
        "Content-Disposition": `attachment; filename="${filename}"`,
      },
    });
  } catch (e) {
    return new Response(JSON.stringify({ error: String(e) }), {
      status: 502,
      headers: { "Content-Type": "application/json" },
    });
  }
}
