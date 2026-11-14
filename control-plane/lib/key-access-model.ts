// Key access: where each API key was used from, and where it was refused,
// in a window. The gateway answers one row per key, network and country
// (GET /usage/key-sources) and one per key, reason and address for refusals
// (GET /usage/key-rejections); this groups them per key for the page.

import type { KeyRejectionAgg, KeySource } from "@/lib/obleth";

export type KeyAccessRange = "24h" | "7d" | "30d";

export const KEY_ACCESS_RANGES: { id: KeyAccessRange; label: string; ms: number; words: string }[] = [
  { id: "24h", label: "24h", ms: 24 * 3_600_000, words: "the last 24 hours" },
  { id: "7d", label: "7d", ms: 7 * 86_400_000, words: "the last 7 days" },
  // The gateway allows 31 days; 30 leaves room for clock drift.
  { id: "30d", label: "30d", ms: 30 * 86_400_000, words: "the last 30 days" },
];

export const isKeyAccessRange = (v: unknown): v is KeyAccessRange => KEY_ACCESS_RANGES.some((r) => r.id === v);
export const keyAccessRange = (id: KeyAccessRange) => KEY_ACCESS_RANGES.find((r) => r.id === id) ?? KEY_ACCESS_RANGES[0];

/** One network and country a key was used from. */
export interface KeyPlace {
  network: string;
  country: string;
  requests: number;
  distinctIps: number;
  lastSeenMs: number;
  /** First seen from this network inside the window. */
  newNetwork: boolean;
  /** First seen from this country inside the window. */
  newCountry: boolean;
}

/** One key's use in the window. */
export interface KeyAccessRow {
  keyId: string;
  keyName: string;
  tenantId: string;
  tenantName: string;
  requests: number;
  /** Distinct addresses, summed over networks (an address is in one network). */
  distinctIps: number;
  lastSeenMs: number;
  /** Busiest first. */
  places: KeyPlace[];
  countries: string[];
  newCountries: string[];
  newNetworks: number;
  refused: number;
}

export interface KeyAccessSummary {
  keys: number;
  networks: number;
  countries: number;
  /** Keys seen from a country they had never been seen from before this window. */
  keysInNewCountry: number;
  refused: number;
  refusedKeys: number;
}

/** Group the per-network rows by key, busiest key first, and mark what is new in the window. */
export function keyAccessRows(sources: KeySource[], rejections: KeyRejectionAgg[], sinceMs: number): KeyAccessRow[] {
  const byKey = new Map<string, KeyAccessRow>();
  for (const s of sources) {
    const row =
      byKey.get(s.key_id) ??
      ({
        keyId: s.key_id,
        keyName: s.key_name,
        tenantId: s.tenant_id,
        tenantName: s.tenant_name,
        requests: 0,
        distinctIps: 0,
        lastSeenMs: 0,
        places: [],
        countries: [],
        newCountries: [],
        newNetworks: 0,
        refused: 0,
      } satisfies KeyAccessRow);
    const country = s.client_country.toUpperCase();
    const newCountry = !!country && s.first_seen_ms !== null && s.first_seen_ms >= sinceMs;
    const newNetwork = s.network_first_seen_ms !== null && s.network_first_seen_ms >= sinceMs;
    row.places.push({
      network: s.network,
      country,
      requests: s.requests,
      distinctIps: s.distinct_ips,
      lastSeenMs: s.last_seen_ms,
      newNetwork,
      newCountry,
    });
    row.requests += s.requests;
    row.distinctIps += s.distinct_ips;
    row.lastSeenMs = Math.max(row.lastSeenMs, s.last_seen_ms);
    if (country && !row.countries.includes(country)) row.countries.push(country);
    if (newCountry && !row.newCountries.includes(country)) row.newCountries.push(country);
    byKey.set(s.key_id, row);
  }
  for (const row of byKey.values()) {
    row.places.sort((a, b) => b.requests - a.requests || b.lastSeenMs - a.lastSeenMs);
    row.newNetworks = new Set(row.places.filter((p) => p.newNetwork).map((p) => p.network)).size;
    row.countries.sort();
  }
  for (const r of rejections) {
    const row = byKey.get(r.key_id);
    if (row) row.refused += r.count;
  }
  return [...byKey.values()].sort((a, b) => b.requests - a.requests || b.lastSeenMs - a.lastSeenMs);
}

export function keyAccessSummary(rows: KeyAccessRow[], rejections: KeyRejectionAgg[]): KeyAccessSummary {
  return {
    keys: rows.length,
    networks: new Set(rows.flatMap((r) => r.places.map((p) => p.network))).size,
    countries: new Set(rows.flatMap((r) => r.countries)).size,
    keysInNewCountry: rows.filter((r) => r.newCountries.length > 0).length,
    refused: rejections.reduce((n, r) => n + r.count, 0),
    refusedKeys: new Set(rejections.map((r) => r.key_id)).size,
  };
}

export type KeyAccessSort = "requests" | "networks" | "recent" | "new";

/** Rows matching a search over key, tenant, network and country, in the chosen order. */
export function filterKeyAccess(rows: KeyAccessRow[], search: string, sort: KeyAccessSort, onlyNew: boolean): KeyAccessRow[] {
  const q = search.trim().toLowerCase();
  const hit = (r: KeyAccessRow) =>
    !q ||
    r.keyName.toLowerCase().includes(q) ||
    r.tenantName.toLowerCase().includes(q) ||
    r.keyId.startsWith(q) ||
    r.countries.some((c) => c.toLowerCase() === q) ||
    r.places.some((p) => p.network.startsWith(q));
  const out = rows.filter((r) => hit(r) && (!onlyNew || r.newCountries.length > 0 || r.newNetworks > 0));
  const by: Record<KeyAccessSort, (a: KeyAccessRow, b: KeyAccessRow) => number> = {
    requests: (a, b) => b.requests - a.requests,
    networks: (a, b) => b.places.length - a.places.length || b.requests - a.requests,
    recent: (a, b) => b.lastSeenMs - a.lastSeenMs,
    new: (a, b) => b.newCountries.length - a.newCountries.length || b.newNetworks - a.newNetworks || b.requests - a.requests,
  };
  return [...out].sort(by[sort]);
}

/** Refusals, most recent first. */
export function refusalRows(rejections: KeyRejectionAgg[]): KeyRejectionAgg[] {
  return [...rejections].sort((a, b) => b.last_seen_ms - a.last_seen_ms);
}

export const REFUSAL_REASON: Record<string, string> = {
  disabled: "Key turned off",
  ip_not_allowed: "Outside allowed networks",
};

/** "US" -> "United States", falling back to the code; empty -> "Unknown". */
export function countryLabel(code: string): string {
  if (!code) return "Unknown";
  try {
    return new Intl.DisplayNames(["en"], { type: "region" }).of(code.toUpperCase()) ?? code;
  } catch {
    return code;
  }
}
