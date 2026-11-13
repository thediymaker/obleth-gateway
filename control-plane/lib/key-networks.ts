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

/** A few words for where a key may be used from. */
export function networksSummary(networks: readonly string[] | null | undefined): string {
  const n = networks?.length ?? 0;
  if (n === 0) return "Any network";
  return n === 1 ? `Only ${networks![0]}` : `Only ${n} networks`;
}
