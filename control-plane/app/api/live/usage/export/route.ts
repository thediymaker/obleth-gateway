import { obleth, type UsageDailyGroupBy } from "@/lib/obleth";
import { liveRoute } from "@/lib/auth/guard";
import { strParam, utcDay } from "@/lib/log-params";
import { buildUsageCsv, selectColumns } from "@/lib/usage-export";

export const GET = liveRoute(async (req) => {
  const p = req.nextUrl.searchParams;
  const startDay = strParam(p, "start_day") ?? utcDay(7);
  const endDay = strParam(p, "end_day") ?? utcDay();
  const groupBy = (strParam(p, "group_by") as UsageDailyGroupBy | undefined) ?? "day";
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
      tenantId: strParam(p, "tenant_id"),
      keyId: strParam(p, "key_id"),
      model: strParam(p, "model"),
    }),
    needTenant ? obleth.listTenants().catch(() => []) : Promise.resolve([]),
    needKey ? obleth.listKeyFields(["id", "name", "key_prefix"] as const).catch(() => []) : Promise.resolve([]),
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
});
