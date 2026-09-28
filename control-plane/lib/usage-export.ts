// Pure CSV-building logic for the usage export route. Kept out of the route
// handler so vitest can cover column selection, name resolution, and cell
// formatting without a Next request context.
import type { UsageDailyRow } from "@/lib/obleth";

/// Every column the export can emit, in display order. `day` is the row's own
/// date for the per-day groupings (blank otherwise); `start_day`/`end_day`
/// carry the range the export covers.
export const ALL_COLUMNS = [
  "day",
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

/** What each column is called on the page. */
export const COLUMN_LABELS: Record<ExportColumn, string> = {
  day: "Date",
  start_day: "Start date",
  end_day: "End date",
  tenant_id: "Team ID",
  tenant_name: "Team",
  key_id: "Key ID",
  key_name: "Key name",
  key_prefix: "Key prefix",
  model: "Model",
  requests: "Requests",
  success_requests: "Succeeded",
  error_requests: "Failed",
  input_tokens: "Tokens in",
  output_tokens: "Tokens out",
  total_tokens: "Total tokens",
  estimated_tokens: "Estimated tokens",
  cache_hits: "Cache hits",
  cache_misses: "Cache misses",
  avg_ttft_ms: "Avg first token (ms)",
  avg_total_ms: "Avg total time (ms)",
  cost_usd: "Spend (USD)",
  energy_kwh: "Energy (kWh)",
  co2_g: "CO₂ (g)",
  energy_cost_usd: "Energy cost (USD)",
};

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

/// Numbers are emitted verbatim. Strings (tenant/key names are user-supplied)
/// that a spreadsheet would evaluate as a formula get a leading `'` so the
/// cell opens as text.
export function csvField(value: string | number): string {
  if (typeof value === "number") return String(value);
  const s = /^[=+\-@\t\r]/.test(value) ? `'${value}` : value;
  return s !== value || /[",\n\r]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}

/// Parse the client's `columns` allowlist. Output order always follows
/// ALL_COLUMNS; an absent or fully-unknown request emits every column.
export function selectColumns(requested: string | null): ExportColumn[] {
  const selected = requested
    ? ALL_COLUMNS.filter((c) => requested.split(",").includes(c))
    : [...ALL_COLUMNS];
  return selected.length > 0 ? selected : [...ALL_COLUMNS];
}

/** One cell as the CSV writes it, before quoting: the preview shows the same. */
export function exportCell(row: UsageDailyRow, col: ExportColumn, ctx: ExportContext): string | number {
  switch (col) {
    case "day":
      return row.day;
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
    ...rows.map((row) => columns.map((c) => csvField(exportCell(row, c, ctx))).join(",")),
  ];
  return lines.join("\r\n");
}
