// Pure CSV-building logic for the usage export route. Kept out of the route
// handler so vitest can cover column selection, name resolution, and cell
// formatting without a Next request context.
import type { UsageDailyRow } from "@/lib/obleth";

/// Every column the export can emit, in display order. Rows are grouped
/// across the whole range, so there is no per-row `day`; `start_day`/`end_day`
/// carry the range each row covers.
export const ALL_COLUMNS = [
  "start_day",
  "end_day",
  "tenant_id",
  "tenant_name",
  "key_id",
  "key_name",
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
  "cost_usd",
  "energy_kwh",
  "co2_g",
  "energy_cost_usd",
] as const;

export type ExportColumn = (typeof ALL_COLUMNS)[number];

export const EMPTY_UUID = "00000000-0000-0000-0000-000000000000";

/// Name lookups make the export human-readable. A missing entry degrades to
/// a blank cell — the id column still carries the identity.
export interface ExportContext {
  startDay: string;
  endDay: string;
  tenantNames: Map<string, string>;
  keyNames: Map<string, string>;
  keyPrefixes: Map<string, string>;
}

export function csvField(value: string | number): string {
  const s = String(value);
  return /[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}

/// Parse the client's `columns` allowlist. Output order always follows
/// ALL_COLUMNS; an absent or fully-unknown request emits every column.
export function selectColumns(requested: string | null): ExportColumn[] {
  const selected = requested
    ? ALL_COLUMNS.filter((c) => requested.split(",").includes(c))
    : [...ALL_COLUMNS];
  return selected.length > 0 ? selected : [...ALL_COLUMNS];
}

function cellValue(row: UsageDailyRow, col: ExportColumn, ctx: ExportContext): string | number {
  switch (col) {
    case "start_day":
      return ctx.startDay;
    case "end_day":
      return ctx.endDay;
    case "tenant_name":
      return row.tenant_id === EMPTY_UUID ? "" : ctx.tenantNames.get(row.tenant_id) ?? "";
    case "key_name":
      return row.key_id === EMPTY_UUID ? "" : ctx.keyNames.get(row.key_id) ?? "";
    case "key_prefix":
      return row.key_id === EMPTY_UUID ? "" : ctx.keyPrefixes.get(row.key_id) ?? "";
    case "tenant_id":
      return row.tenant_id === EMPTY_UUID ? "" : row.tenant_id;
    case "key_id":
      return row.key_id === EMPTY_UUID ? "" : row.key_id;
    case "energy_kwh":
      return Number((row.energy_wh / 1000).toFixed(4));
    default:
      // cost_usd, co2_g, energy_cost_usd, and all count/latency columns are
      // stored values emitted verbatim (cost is frozen at completion — never
      // recomputed here).
      return (row[col] ?? "") as string | number;
  }
}

export function buildUsageCsv(
  rows: UsageDailyRow[],
  columns: ExportColumn[],
  ctx: ExportContext,
): string {
  const lines = [
    columns.join(","),
    ...rows.map((row) => columns.map((c) => csvField(cellValue(row, c, ctx))).join(",")),
  ];
  return lines.join("\r\n");
}
