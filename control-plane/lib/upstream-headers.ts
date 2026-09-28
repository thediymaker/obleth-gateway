// The model form's "Upstream headers" textarea, one `Name: value` per line.
// Header values are write-only in the gateway API (an operator may put a
// credential in one), so the edit form can only show the stored names. A line
// with a name and no value therefore means "keep the stored value", sent as
// `null`; a line with a value sets it; a stored name whose line was deleted is
// removed. Validation (denied names, bad characters) is the gateway's, so a
// rejected header surfaces as a save error rather than being dropped here.

export type UpstreamHeadersWrite = Record<string, string | null>;

export function parseUpstreamHeaders(raw: string): UpstreamHeadersWrite {
  const out: UpstreamHeadersWrite = {};
  for (const line of raw.split(/\r?\n/)) {
    if (!line.trim()) continue;
    const colon = line.indexOf(":");
    const name = (colon === -1 ? line : line.slice(0, colon)).trim();
    const value = colon === -1 ? "" : line.slice(colon + 1).trim();
    if (!name) continue;
    out[name] = value === "" ? null : value;
  }
  return out;
}

// The edit form's starting text: every stored name, value left blank (kept).
export function upstreamHeadersText(names: readonly string[] | undefined): string {
  return (names ?? []).map((name) => `${name}:`).join("\n");
}
