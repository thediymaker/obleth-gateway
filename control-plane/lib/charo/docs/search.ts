import type { DocChunk, DocsIndex, DocsSource } from "./types";

const STOPWORDS = new Set([
  "the", "a", "an", "of", "to", "in", "on", "for", "and", "or", "is", "are",
  "do", "i", "how", "my", "with", "it", "this", "that", "can", "you", "your",
]);

// Drop results scoring below this fraction of the top hit. Tunable.
const RELEVANCE_RATIO = 0.4;

function tokenize(s: string): string[] {
  return (s.toLowerCase().match(/[a-z0-9]+/g) ?? []).filter(
    (t) => t.length >= 2 && !STOPWORDS.has(t),
  );
}

function count(tokens: string[], term: string): number {
  let n = 0;
  for (const t of tokens) if (t === term) n++;
  return n;
}

/** Flatten genuine GFM tables into readable prose; leave other `|` alone. */
function flattenMarkup(text: string): string {
  const lines = text.split("\n");
  const n = lines.length;

  // Mark lines inside fenced code blocks (``` or ~~~); never table rows.
  const fenced: boolean[] = new Array(n).fill(false);
  let inFence = false;
  let marker = "";
  for (let i = 0; i < n; i++) {
    const m = lines[i].match(/^\s*(```|~~~)/);
    if (m) {
      fenced[i] = true;
      if (!inFence) { inFence = true; marker = m[1]; }
      else if (m[1] === marker) { inFence = false; }
    } else {
      fenced[i] = inFence;
    }
  }

  const isSep = (line: string): boolean =>
    line.includes("|") &&
    line.trim().replace(/^\||\|$/g, "").split("|").every((c) => /^\s*:?-{2,}:?\s*$/.test(c));

  // A real table row is edge-piped (leading or trailing `|`). Prose/inline pipes
  // (shell `a | b`, regex `a|b`) are not, so they are never swept into a table.
  const isRow = (line: string): boolean =>
    !isSep(line) && line.includes("|") && (/^\s*\|/.test(line) || /\|\s*$/.test(line));

  // Each separator row plus its header (line above) and contiguous body rows below.
  const table: boolean[] = new Array(n).fill(false);
  for (let i = 0; i < n; i++) {
    if (fenced[i] || !isSep(lines[i])) continue;
    table[i] = true; // separator row (dropped below)
    if (i - 1 >= 0 && !fenced[i - 1] && isRow(lines[i - 1])) table[i - 1] = true;
    for (let k = i + 1; k < n && !fenced[k] && isRow(lines[k]); k++) table[k] = true;
  }

  return lines
    .map((line, i) => {
      if (!table[i]) return line;   // prose / code: leave `|` untouched
      if (isSep(line)) return "";   // drop the separator row
      return line
        .replace(/^\s*\|\s*/, "")
        .replace(/\s*\|\s*$/, "")
        .replace(/\s*\|\s*/g, " · ");
    })
    .join(" ")
    .replace(/\s+/g, " ")
    .trim();
}

/** Excerpt ~200 chars centered on the first query hit, snapped to word bounds. */
function snippet(text: string, terms: string[]): string {
  const clean = flattenMarkup(text);
  const lower = clean.toLowerCase();
  let at = -1;
  for (const t of terms) {
    const i = lower.indexOf(t);
    if (i !== -1 && (at === -1 || i < at)) at = i;
  }
  let start = at <= 0 ? 0 : Math.max(0, at - 60);
  let end = Math.min(clean.length, start + 200);
  // Snap outward to whitespace so the excerpt never cuts mid-token. Guard the
  // search distance so a long unbroken token falls back to the raw offset.
  if (start > 0 && clean[start - 1] !== " ") {
    const ws = clean.indexOf(" ", start);
    if (ws !== -1 && ws - start <= 40) start = ws + 1;
  }
  if (end < clean.length) {
    const ws = clean.lastIndexOf(" ", end);
    if (ws > start && end - ws <= 40) end = ws;
  }
  const raw = clean.slice(start, end).trim();
  return (start > 0 ? "..." : "") + raw + (end < clean.length ? "..." : "");
}

/**
 * Deterministic BM25-lite ranking over the bundled index. Terms hitting a
 * chunk's title/heading are weighted 3x; contributions saturate so a single
 * flooded term can't dominate. Returns up to `limit` sources with score > 0.
 */
export function searchDocs(index: DocsIndex, query: string, limit: number): DocsSource[] {
  const qTerms = [...new Set(tokenize(query))];
  if (qTerms.length === 0 || index.chunks.length === 0) return [];

  const bodyTokens = index.chunks.map((c) => tokenize(c.text));
  const fieldTokens = index.chunks.map((c) => tokenize(`${c.title} ${c.heading}`));

  const N = index.chunks.length;
  const df = new Map<string, number>();
  for (const term of qTerms) {
    let d = 0;
    for (let i = 0; i < N; i++) {
      if (count(bodyTokens[i], term) + count(fieldTokens[i], term) > 0) d++;
    }
    df.set(term, d);
  }

  const scored = index.chunks.map((chunk: DocChunk, i: number) => {
    let score = 0;
    for (const term of qTerms) {
      const d = df.get(term) ?? 0;
      if (d === 0) continue;
      const idf = Math.log(1 + N / d);
      const tf = count(bodyTokens[i], term) + 3 * count(fieldTokens[i], term);
      if (tf > 0) score += idf * (tf / (tf + 1));
    }
    return { chunk, score };
  });

  const ranked = scored
    .filter((s) => s.score > 0)
    .sort(
      (a, b) =>
        b.score - a.score ||
        (a.chunk.id < b.chunk.id ? -1 : a.chunk.id > b.chunk.id ? 1 : 0),
    );

  if (ranked.length === 0) return [];
  const cutoff = ranked[0].score * RELEVANCE_RATIO;
  return ranked
    .filter((s) => s.score >= cutoff)
    .slice(0, Math.max(0, limit))
    .map(({ chunk }) => ({
      route: chunk.route,
      title: chunk.title,
      heading: chunk.heading,
      snippet: snippet(chunk.text, qTerms),
    }));
}
