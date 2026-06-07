import { NextRequest } from "next/server";
import { obleth } from "@/lib/obleth";
import type { UsageDailyGroupBy, UsageDailyRow } from "@/lib/obleth";

// Every column the export can emit, in display order. The client sends a
// `columns` allowlist (checkboxes); when absent we emit them all. Rows are
// grouped per key+model across the whole range, so there is no per-row `day`;
// instead `start_day`/`end_day` carry the range each row covers.
const ALL_COLUMNS = [
  "start_day",
  "end_day",
  "tenant_id",
  "tenant_name",
  "key_id",
  "key_prefix",
  "model",
  "requests",
  "success_requests",
  "error_requests",
  "input_tokens",
  "output_tokens",
  "total_tokens",
  "estimated_tokens",
  "cache_hits",
  "cache_misses",
  "avg_ttft_ms",
  "avg_total_ms",
] as const;

type Column = (typeof ALL_COLUMNS)[number];

const EMPTY_UUID = "00000000-0000-0000-0000-000000000000";

function csvField(value: string | number): string {
  const s = String(value);
  return /[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}

function defaultStart(): string {
  return new Date(Date.now() - 7 * 86_400_000).toISOString().slice(0, 10);
}

function today(): string {
  return new Date().toISOString().slice(0, 10);
}

export async function GET(req: NextRequest) {
  try {
    const p = req.nextUrl.searchParams;
    const startDay = p.get("start_day") ?? defaultStart();
    const endDay = p.get("end_day") ?? today();
    const groupBy = (p.get("group_by") as UsageDailyGroupBy | null) ?? "day";

    const requested = p.get("columns");
    const selected: Column[] = requested
      ? ALL_COLUMNS.filter((c) => requested.split(",").includes(c))
      : [...ALL_COLUMNS];
    const columns = selected.length > 0 ? selected : [...ALL_COLUMNS];

    // Name lookups make the export human-readable. Failure to resolve a name is
    // non-fatal — the id column still carries the identity.
    const needTenant = columns.includes("tenant_name");
    const needKey = columns.includes("key_prefix");
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

    const tenantNames = new Map(tenants.map((t) => [t.id, t.name]));
    const keyPrefixes = new Map(keys.map((k) => [k.id, k.key_prefix]));

    const cell = (row: UsageDailyRow, col: Column): string | number => {
      switch (col) {
        case "start_day":
          return startDay;
        case "end_day":
          return endDay;
        case "tenant_name":
          return row.tenant_id === EMPTY_UUID ? "" : tenantNames.get(row.tenant_id) ?? "";
        case "key_prefix":
          return row.key_id === EMPTY_UUID ? "" : keyPrefixes.get(row.key_id) ?? "";
        case "tenant_id":
          return row.tenant_id === EMPTY_UUID ? "" : row.tenant_id;
        case "key_id":
          return row.key_id === EMPTY_UUID ? "" : row.key_id;
        default:
          return (row[col] ?? "") as string | number;
      }
    };

    const lines = [
      columns.join(","),
      ...rows.map((row) => columns.map((c) => csvField(cell(row, c))).join(",")),
    ];
    const csv = lines.join("\r\n");
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
