import { oursFirst, splitAllowedNetworks } from "@/lib/named-networks";
import type { NamedNetwork } from "@/lib/obleth";

/** Helper text under every "Allowed networks" field. */
export const NETWORKS_HINT =
  "One CIDR range or address per line. Leave empty to allow any network. Requests from anywhere else are refused.";

/**
 * A key's allowed networks as typed into a form: one CIDR range or address
 * per line (commas work too). Blank lines go; the gateway checks each entry
 * and stores it in canonical form, so nothing else is judged here.
 */
export function networkLines(raw: FormDataEntryValue | string | null | undefined): string[] {
  if (typeof raw !== "string") return [];
  return raw
    .split(/[\n,]/)
    .map((line) => line.trim())
    .filter(Boolean);
}

/** The list as the form shows it: one entry per line. */
export function networksText(networks: readonly string[] | null | undefined): string {
  return (networks ?? []).join("\n");
}

/** Helper text under every "Allowed countries" field. */
export const COUNTRIES_HINT =
  "Requests from the internet in these countries are allowed too. Requests from private networks (campus, VPN, pods) have no country, so allow those by network.";

/**
 * A key's allowed countries as typed into a form: two-letter codes
 * separated by commas or spaces, upper-cased, repeats dropped. The gateway
 * checks each code, so nothing is refused here.
 */
export function countryCodes(raw: FormDataEntryValue | string | null | undefined): string[] {
  if (typeof raw !== "string") return [];
  const codes = raw
    .split(/[\s,]+/)
    .map((c) => c.trim().toUpperCase())
    .filter(Boolean);
  return [...new Set(codes)];
}

/** The list as the form shows it. */
export function countriesText(countries: readonly string[] | null | undefined): string {
  return (countries ?? []).join(", ");
}

let regionNames: Intl.DisplayNames | null | undefined;

/**
 * The English name of a two-letter country code (`US` is "United States"),
 * or `null` when it is not one: not two letters, `XX` (the edge's
 * "unknown"), or a code no country has.
 */
export function countryName(code: string): string | null {
  const c = code.trim().toUpperCase();
  if (!/^[A-Z]{2}$/.test(c) || c === "XX") return null;
  if (regionNames === undefined) {
    try {
      regionNames = new Intl.DisplayNames(["en"], { type: "region", fallback: "none" });
    } catch {
      regionNames = null;
    }
  }
  const name = regionNames?.of(c);
  return name && name !== c ? name : null;
}

/** A country as it reads mid-sentence: "the United States", "Mexico", or the code when it has no name. */
function countryInSentence(code: string): string {
  const name = countryName(code);
  if (!name) return code;
  return /^(United .*|Netherlands|Philippines|Bahamas)$/.test(name) ? `the ${name}` : name;
}

/**
 * A few words for where a key may be used from. Given the named networks
 * (Settings › Networks), a network whose every range is listed is called by
 * its name: "Only ASU campus, Sol and 2 more", "Only ASU campus or the
 * United States". Without them, ranges and countries are counted.
 */
export function networksSummary(
  networks: readonly string[] | null | undefined,
  countries?: readonly string[] | null,
  named?: readonly NamedNetwork[] | null,
): string {
  const n = networks?.length ?? 0;
  const c = countries?.length ?? 0;
  if (n === 0 && c === 0) return "Any network";
  if (named?.length) {
    const { names, rest } = splitAllowedNetworks(networks ?? [], oursFirst([...named]));
    const parts = [...names, ...rest, ...(countries ?? []).map(countryInSentence)];
    if (parts.length > 3) return `Only ${parts.slice(0, 2).join(", ")} and ${parts.length - 2} more`;
    if (parts.length === 1) return `Only ${parts[0]}`;
    return `Only ${parts.slice(0, -1).join(", ")} or ${parts[parts.length - 1]}`;
  }
  const parts: string[] = [];
  if (n > 0) parts.push(n === 1 ? networks![0] : `${n} networks`);
  if (c > 0) parts.push(c === 1 ? countries![0] : `${c} countries`);
  return `Only ${parts.join(" or ")}`;
}
