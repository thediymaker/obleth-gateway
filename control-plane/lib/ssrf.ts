import { BlockList, isIP } from "node:net";
import { lookup } from "node:dns/promises";

// Mirrors the gateway's always-blocked set (obleth-admin/src/ssrf.rs): private
// and loopback ranges stay reachable (on-prem product), but link-local, the
// unspecified address, and cloud metadata endpoints never are.
const ALWAYS_BLOCKED = new BlockList();
ALWAYS_BLOCKED.addSubnet("169.254.0.0", 16, "ipv4");
ALWAYS_BLOCKED.addAddress("0.0.0.0", "ipv4");
ALWAYS_BLOCKED.addAddress("100.100.100.200", "ipv4");
ALWAYS_BLOCKED.addSubnet("fe80::", 10, "ipv6");
ALWAYS_BLOCKED.addAddress("::", "ipv6");
// IPv4-mapped, IPv4-compatible and NAT64 forms of those addresses are handled
// by classifying them as their embedded IPv4 address (see embeddedIpv4).

/** Expand a valid IPv6 literal into its eight 16-bit groups. */
function ipv6Groups(ipv6: string): number[] {
  // URL parsing canonicalizes every spelling (dotted tails included) to hex groups.
  const canonical = new URL(`http://[${ipv6}]`).hostname.slice(1, -1);
  const [head, tail] = canonical.split("::");
  const parse = (part: string | undefined) => (part ? part.split(":").map((g) => parseInt(g, 16)) : []);
  const hi = parse(head);
  if (tail === undefined) return hi;
  const lo = parse(tail);
  return [...hi, ...new Array(8 - hi.length - lo.length).fill(0), ...lo];
}

/**
 * IPv6 spellings that reach an IPv4 host must be classified as that IPv4
 * address: IPv4-mapped (`::ffff:a.b.c.d`), IPv4-compatible (`::a.b.c.d`) and
 * NAT64 (`64:ff9b::a.b.c.d`). Mirrors `unmap` in obleth-admin/src/ssrf.rs.
 * Returns null for any other IPv6 address.
 */
function embeddedIpv4(ipv6: string): string | null {
  const g = ipv6Groups(ipv6);
  const zeroUpTo = (n: number) => g.slice(0, n).every((x) => x === 0);
  const v4 = ((g[6] << 16) | g[7]) >>> 0;
  const mapped = zeroUpTo(5) && g[5] === 0xffff;
  // `::` and `::1` are the IPv6 unspecified/loopback addresses, not
  // IPv4-compatible forms; they classify as themselves.
  const compat = zeroUpTo(6) && v4 > 1;
  const nat64 = g[0] === 0x64 && g[1] === 0xff9b && g.slice(2, 6).every((x) => x === 0);
  if (!mapped && !compat && !nat64) return null;
  return [g[6] >> 8, g[6] & 0xff, g[7] >> 8, g[7] & 0xff].join(".");
}

/** True when `address` (an IP literal) is in the always-blocked set. Non-IPs are blocked. */
export function isBlockedAddress(address: string): boolean {
  const bare = address.replace(/^\[|\]$/g, "").replace(/%.*$/, "");
  const family = isIP(bare);
  if (family === 0) return true;
  if (family === 6) {
    const v4 = embeddedIpv4(bare);
    if (v4) return ALWAYS_BLOCKED.check(v4, "ipv4");
  }
  return ALWAYS_BLOCKED.check(bare, family === 4 ? "ipv4" : "ipv6");
}

/**
 * Resolve `hostname` and return an error message if ANY address it resolves to
 * is blocked (so a name with one public and one metadata record is refused),
 * or null when it is safe to fetch.
 */
export async function blockedHostReason(hostname: string): Promise<string | null> {
  const host = hostname.replace(/^\[|\]$/g, "");
  let addresses: { address: string }[];
  try {
    addresses = await lookup(host, { all: true });
  } catch {
    return `Could not resolve ${host}.`;
  }
  if (addresses.length === 0 || addresses.some((a) => isBlockedAddress(a.address))) {
    return "Provider URL resolves to a link-local or metadata address, which is not allowed.";
  }
  return null;
}
