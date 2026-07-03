// Pure labeling/sorting for the Reports breakdown table. Kept out of the
// component so grouping behavior is vitest-testable.
import type { UsageDailyRow } from "@/lib/obleth";

export type BreakdownGroup = "day" | "tenant" | "key" | "model";

export interface NameLookups {
  tenantNames: Map<string, string>;
  keyNames: Map<string, string>;
  keyPrefixes: Map<string, string>;
}

export interface BreakdownRow extends UsageDailyRow {
  label: string;
  sublabel: string;
}

/// Label rollup rows for display. Day rows stay chronological; every other
/// grouping is a chargeback view, so it sorts by frozen spend descending.
/// Name lookups degrade to a truncated raw id (deleted tenants/keys).
export function toBreakdownRows(
  rows: UsageDailyRow[],
  groupBy: BreakdownGroup,
  lookups: NameLookups,
): BreakdownRow[] {
  const labeled = rows.map((r): BreakdownRow => {
    let label = "";
    let sublabel = "";
    switch (groupBy) {
      case "day":
        label = r.day;
        break;
      case "tenant":
        label = lookups.tenantNames.get(r.tenant_id) ?? r.tenant_id.slice(0, 8);
        break;
      case "key":
        label = lookups.keyNames.get(r.key_id) || r.key_id.slice(0, 8);
        sublabel = lookups.keyPrefixes.get(r.key_id) ?? "";
        break;
      case "model":
        label = r.model || "(unknown)";
        break;
    }
    return { ...r, label, sublabel };
  });
  if (groupBy === "day") {
    return labeled.sort((a, b) => a.day.localeCompare(b.day));
  }
  return labeled.sort((a, b) => b.cost_usd - a.cost_usd);
}

/// Aggregate-spend formatter for report tables/KPIs: em-dash for zero (matches
/// the energy/CO₂ cells) and 2-decimal dollars. Per-request costs elsewhere use
/// lib/utils.ts formatCurrency, which keeps up to 6 decimals for tiny amounts.
export function formatUsd(v: number): string {
  if (v <= 0) return "—";
  if (v < 0.01) return "< $0.01";
  return `$${v.toFixed(2)}`;
}
