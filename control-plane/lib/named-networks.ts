// Named networks: ranges of addresses an operator names (a campus, a VPN, a
// cluster's pods) so reports can say whose network a client is in. The
// gateway validates and normalises them; this catches the obvious mistakes
// before a save and keeps the editor's rows in one shape.

import type { NamedNetwork } from "@/lib/obleth";

export const NETWORK_NAME_MAX = 64;
export const NAMED_NETWORKS_MAX = 500;

/** Whether text looks like an IPv4 or IPv6 address or CIDR range. The gateway has the final say. */
export function looksLikeNetwork(raw: string): boolean {
  const [addr, prefix, extra] = raw.trim().split("/");
  if (!addr || extra !== undefined) return false;
  if (prefix !== undefined && !/^\d{1,3}$/.test(prefix)) return false;
  const bits = prefix === undefined ? null : Number(prefix);
  if (addr.includes(":")) return /^[0-9a-fA-F:.]+$/.test(addr) && addr.split("::").length <= 2 && (bits === null || bits <= 128);
  const parts = addr.split(".");
  return parts.length === 4 && parts.every((p) => /^\d{1,3}$/.test(p) && Number(p) <= 255) && (bits === null || bits <= 32);
}

/** The rows that will be saved: blank rows dropped, text trimmed. */
export function cleanNamedNetworks(rows: NamedNetwork[]): NamedNetwork[] {
  return rows.map((r) => ({ network: r.network.trim(), name: r.name.trim() })).filter((r) => r.network || r.name);
}

/** What is wrong with the rows, in words, or null. */
export function namedNetworksProblem(rows: NamedNetwork[]): string | null {
  const clean = cleanNamedNetworks(rows);
  const seen = new Set<string>();
  for (const r of clean) {
    if (!looksLikeNetwork(r.network)) return r.network ? `“${r.network}” is not an address or range, such as 10.0.0.0/8.` : `${r.name} needs a range.`;
    if (!r.name) return `${r.network} needs a name.`;
    if (r.name.length > NETWORK_NAME_MAX) return `The name for ${r.network} is longer than ${NETWORK_NAME_MAX} characters.`;
    const key = r.network.toLowerCase();
    if (seen.has(key)) return `${r.network} is listed twice.`;
    seen.add(key);
  }
  if (clean.length > NAMED_NETWORKS_MAX) return `At most ${NAMED_NETWORKS_MAX} named networks.`;
  return null;
}

/** Parse the editor's hidden field; anything unreadable is no rows. */
export function parseNamedNetworksField(raw: unknown): NamedNetwork[] | null {
  if (typeof raw !== "string") return null;
  try {
    const v = JSON.parse(raw) as unknown;
    if (!Array.isArray(v)) return null;
    return v
      .filter((r): r is NamedNetwork => !!r && typeof r === "object" && typeof (r as NamedNetwork).network === "string" && typeof (r as NamedNetwork).name === "string")
      .map((r) => ({ network: r.network, name: r.name }));
  } catch {
    return null;
  }
}
