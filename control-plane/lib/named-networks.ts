// Named networks: groups of address ranges an operator names (a campus, a
// VPN, a cluster's pods) so reports can say whose network a client is in.
// The gateway validates, normalises and merges them; this catches the
// obvious mistakes before a save and keeps the editor's groups in one shape.

import type { NamedNetwork } from "@/lib/obleth";

export const NETWORK_NAME_MAX = 64;
/** Most ranges across all named networks. */
export const NAMED_NETWORKS_MAX = 500;
/** Most named networks. */
export const NAMED_NETWORK_GROUPS_MAX = 200;

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

/** Ranges typed one per line (commas work too), trimmed, blanks dropped. */
export function rangeLines(text: string): string[] {
  return text
    .split(/[\n,]/)
    .map((l) => l.trim())
    .filter(Boolean);
}

/** The groups that will be saved: names and ranges trimmed, blank ranges and blank groups dropped. */
export function cleanNamedNetworks(groups: NamedNetwork[]): NamedNetwork[] {
  return groups
    .map((g) => ({ name: g.name.trim(), ranges: g.ranges.map((r) => r.trim()).filter(Boolean), ours: !!g.ours }))
    .filter((g) => g.name || g.ranges.length);
}

/**
 * What is wrong with each group, in words, or null, in the order given
 * (blank groups have no problem). Problems that concern the whole list
 * (too many) are reported on the last group.
 */
export function namedNetworkProblems(groups: NamedNetwork[]): (string | null)[] {
  const names = new Map<string, string>();
  const owners = new Map<string, string>();
  let total = 0;
  let count = 0;
  const out = groups.map((raw) => {
    const [g] = cleanNamedNetworks([raw]);
    if (!g) return null;
    count += 1;
    total += g.ranges.length;
    const label = g.name || g.ranges[0]!;
    if (!g.name) return `${label} needs a name.`;
    if (g.name.length > NETWORK_NAME_MAX) return `The name ${g.name.slice(0, 24)}... is longer than ${NETWORK_NAME_MAX} characters.`;
    const key = g.name.toLowerCase();
    if (names.has(key)) return `Another network is already called ${names.get(key)}.`;
    names.set(key, g.name);
    if (!g.ranges.length) return `${g.name} needs at least one address or range.`;
    const bad = g.ranges.find((r) => !looksLikeNetwork(r));
    if (bad) return `${g.name}: “${bad}” is not an address or range, such as 10.0.0.0/8.`;
    for (const r of g.ranges) {
      const k = r.toLowerCase();
      const other = owners.get(k);
      if (other !== undefined && other !== g.name) return `${r} is in both ${other} and ${g.name}. A range can belong to one network only.`;
      owners.set(k, g.name);
    }
    return null;
  });
  const last = out.length - 1;
  if (last >= 0 && !out[last]) {
    if (count > NAMED_NETWORK_GROUPS_MAX) out[last] = `At most ${NAMED_NETWORK_GROUPS_MAX} named networks.`;
    else if (total > NAMED_NETWORKS_MAX) out[last] = `At most ${NAMED_NETWORKS_MAX} ranges across all named networks.`;
  }
  return out;
}

/** The first thing wrong with the groups, in words, or null. */
export function namedNetworksProblem(groups: NamedNetwork[]): string | null {
  return namedNetworkProblems(groups).find((p) => p !== null) ?? null;
}

/**
 * One named network from either shape the gateway accepts: a group
 * (`{ name, ranges, ours }`) or an older one-range row (`{ network, name }`).
 * Anything else is null.
 */
export function toNamedNetwork(raw: unknown): NamedNetwork | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  const name = typeof r.name === "string" ? r.name : "";
  if (typeof r.network === "string") return { name, ranges: [r.network], ours: false };
  if (!Array.isArray(r.ranges) && typeof r.name !== "string") return null;
  const ranges = Array.isArray(r.ranges) ? r.ranges.filter((x): x is string => typeof x === "string") : [];
  return { name, ranges, ours: r.ours === true };
}

/** Parse the editor's hidden field; anything unreadable is null. */
export function parseNamedNetworksField(raw: unknown): NamedNetwork[] | null {
  if (typeof raw !== "string") return null;
  try {
    const v = JSON.parse(raw) as unknown;
    if (!Array.isArray(v)) return null;
    return v.map(toNamedNetwork).filter((g): g is NamedNetwork => g !== null);
  } catch {
    return null;
  }
}

/** How many ranges the groups hold. */
export function rangeCount(groups: NamedNetwork[]): number {
  return groups.reduce((n, g) => n + g.ranges.length, 0);
}

/** Our own networks first, otherwise in the order given. */
export function oursFirst(groups: NamedNetwork[]): NamedNetwork[] {
  return [...groups.filter((g) => g.ours), ...groups.filter((g) => !g.ours)];
}

/**
 * A range as the gateway stores it, near enough to compare: trimmed and
 * lower-cased (IPv6). Both a key's list and the named networks come back
 * canonical, so this is all the matching needs.
 */
export function rangeKey(range: string): string {
  return range.trim().toLowerCase();
}

/**
 * A key's allowed list read back as named networks: a network counts when
 * every one of its ranges is listed, and `rest` is whatever no whole network
 * accounts for, in the order listed.
 */
export function splitAllowedNetworks(listed: readonly string[], groups: readonly NamedNetwork[]): { names: string[]; rest: string[] } {
  const present = new Set(listed.map(rangeKey));
  const whole = groups.filter((g) => g.ranges.length > 0 && g.ranges.every((r) => present.has(rangeKey(r))));
  const taken = new Set(whole.flatMap((g) => g.ranges.map(rangeKey)));
  return { names: whole.map((g) => g.name), rest: listed.filter((n) => !taken.has(rangeKey(n))) };
}

interface Cidr {
  v6: boolean;
  bits: bigint;
  prefix: number;
}

function parseV4(text: string): bigint | null {
  const parts = text.split(".");
  if (parts.length !== 4 || !parts.every((p) => /^\d{1,3}$/.test(p) && Number(p) <= 255)) return null;
  return parts.reduce((n, p) => (n << BigInt(8)) | BigInt(Number(p)), BigInt(0));
}

function parseV6(text: string): bigint | null {
  let s = text.toLowerCase();
  const lastColon = s.lastIndexOf(":");
  if (s.slice(lastColon + 1).includes(".")) {
    // An IPv4 tail (::ffff:192.0.2.1) is two more words.
    const v4 = parseV4(s.slice(lastColon + 1));
    if (v4 === null) return null;
    s = `${s.slice(0, lastColon + 1)}${(v4 >> BigInt(16)).toString(16)}:${(v4 & BigInt(0xffff)).toString(16)}`;
  }
  const halves = s.split("::");
  if (halves.length > 2) return null;
  const words = (h: string) => (h === "" ? [] : h.split(":"));
  const head = words(halves[0]!);
  const back = halves.length === 2 ? words(halves[1]!) : [];
  if (![...head, ...back].every((w) => /^[0-9a-f]{1,4}$/.test(w))) return null;
  const known = head.length + back.length;
  if (halves.length === 1 ? known !== 8 : known > 7) return null;
  return [...head, ...Array<string>(8 - known).fill("0"), ...back].reduce((n, w) => (n << BigInt(16)) | BigInt(parseInt(w, 16)), BigInt(0));
}

function parseCidr(raw: string): Cidr | null {
  const [addr, prefixText, extra] = raw.trim().split("/");
  if (!addr || extra !== undefined) return null;
  const v6 = addr.includes(":");
  const width = v6 ? 128 : 32;
  const bits = v6 ? parseV6(addr) : parseV4(addr);
  if (bits === null) return null;
  const prefix = prefixText === undefined ? width : /^\d{1,3}$/.test(prefixText) ? Number(prefixText) : NaN;
  if (!(prefix >= 0 && prefix <= width)) return null;
  return { v6, bits, prefix };
}

function cidrContains(outer: Cidr, inner: Cidr): boolean {
  if (outer.v6 !== inner.v6 || outer.prefix > inner.prefix) return false;
  const shift = BigInt((outer.v6 ? 128 : 32) - outer.prefix);
  return outer.bits >> shift === inner.bits >> shift;
}

/** The other named networks that hold every range of `group` (a cluster inside a campus). */
export function networksHolding(group: NamedNetwork, groups: readonly NamedNetwork[]): string[] {
  const inner = group.ranges.map(parseCidr).filter((c): c is Cidr => c !== null);
  if (!inner.length) return [];
  return groups
    .filter((g) => g.name !== group.name)
    .filter((g) => {
      const outer = g.ranges.map(parseCidr).filter((c): c is Cidr => c !== null);
      return inner.every((i) => outer.some((o) => cidrContains(o, i)));
    })
    .map((g) => g.name);
}
