import type { DocsSearchResult, DocsSource } from "./types";

interface ToolResultEnvelope {
  type: string;
  data: unknown;
}

/**
 * The brain may call search_docs more than once in a single agent turn, and
 * each call streams its own docs_result — rendering as stacked, largely
 * overlapping source cards. Merge them into one entry (at the first result's
 * position), deduped by route+heading, so a turn shows a single sources rail.
 */
export function coalesceDocsResults(results: ToolResultEnvelope[]): ToolResultEnvelope[] {
  const docs = results.filter((r) => r.type === "docs_result");
  if (docs.length <= 1) return results;

  const seen = new Set<string>();
  const sources: DocsSource[] = [];
  for (const d of docs) {
    const r = (d.data ?? {}) as Partial<DocsSearchResult>;
    if (!Array.isArray(r.sources)) continue;
    for (const s of r.sources) {
      const key = `${s.route}#${s.heading}`;
      if (seen.has(key)) continue;
      seen.add(key);
      sources.push(s);
    }
  }
  const query = (docs[0].data as Partial<DocsSearchResult> | null)?.query ?? "";
  const merged: ToolResultEnvelope = { type: "docs_result", data: { query, sources } };

  let placed = false;
  const out: ToolResultEnvelope[] = [];
  for (const r of results) {
    if (r.type !== "docs_result") {
      out.push(r);
    } else if (!placed) {
      out.push(merged);
      placed = true;
    }
  }
  return out;
}
