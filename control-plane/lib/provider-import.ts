import { normalizeModelApiNameFinal } from "./model-name";

export interface UpstreamModel {
  id: string;
  owned_by?: string;
}

export function normalizeBase(base: string): string {
  return base.trim().replace(/\/+$/, "");
}

// Accepts the OpenAI `{ object:"list", data:[...] }` shape or a bare array.
// Keeps only entries with a non-empty string id, dedupes, sorts ascending.
export function parseUpstreamModelList(json: unknown): UpstreamModel[] {
  const arr: unknown[] = Array.isArray(json)
    ? json
    : json && typeof json === "object" && Array.isArray((json as { data?: unknown }).data)
      ? (json as { data: unknown[] }).data
      : [];

  const seen = new Set<string>();
  const out: UpstreamModel[] = [];
  for (const entry of arr) {
    if (!entry || typeof entry !== "object") continue;
    const id = (entry as { id?: unknown }).id;
    if (typeof id !== "string" || id.trim() === "") continue;
    if (seen.has(id)) continue;
    seen.add(id);
    const ownedBy = (entry as { owned_by?: unknown }).owned_by;
    out.push({ id, owned_by: typeof ownedBy === "string" ? ownedBy : undefined });
  }
  out.sort((a, b) => a.id.localeCompare(b.id));
  return out;
}
