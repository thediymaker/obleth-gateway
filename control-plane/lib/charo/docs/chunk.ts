import type { DocChunk } from "./types";

/** "guides/api-keys/index.mdx" -> "guides/api-keys". */
export function routeFromPath(relPath: string): string {
  return relPath
    .replace(/\\/g, "/")
    .replace(/\/index\.mdx$/i, "")
    .replace(/\.mdx$/i, "")
    .replace(/^\/+/, "");
}

function slug(s: string): string {
  return s
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

interface Frontmatter { title: string; body: string; }

/** Split leading `---\n...\n---` frontmatter; return its title and the remaining body. */
function parseFrontmatter(source: string): Frontmatter {
  const m = source.match(/^\uFEFF?---\n([\s\S]*?)\n---\n?/);
  if (!m) return { title: "", body: source };
  const titleLine = m[1].match(/^title:\s*(.+)$/m);
  const title = titleLine ? titleLine[1].trim().replace(/^["']|["']$/g, "") : "";
  return { title, body: source.slice(m[0].length) };
}

function clean(text: string): string {
  // Strip <img> everywhere first (alt-text blobs are pure noise, even in samples).
  let t = text.replace(/<img\b[^>]*\/?>/gi, "");
  // Mask fenced code blocks and inline code spans so the generic tag-strip below
  // can't delete angle-bracket placeholders (e.g. <OBLETH_ADMIN_TOKEN>) that are
  // real documentation content. NUL sentinels cannot occur in markdown source, so
  // they never collide with prose (space-padded numbers, tables, etc.).
  const kept: string[] = [];
  const mask = (m: string) => { kept.push(m); return `\u0000${kept.length - 1}\u0000`; };
  t = t
    .replace(/```[\s\S]*?```/g, mask)
    .replace(/~~~[\s\S]*?~~~/g, mask)
    .replace(/`[^`\n]*`/g, mask);
  // Strip stray JSX/HTML tags remaining in prose only.
  t = t.replace(/<\/?[A-Za-z][^>]*>/g, "");
  // Restore masked code verbatim.
  t = t.replace(/\u0000(\d+)\u0000/g, (_, i) => kept[Number(i)]);
  return t.replace(/\n{3,}/g, "\n\n").trim();
}

/**
 * Split an MDX document into heading-level chunks. Content before the first
 * `##`/`###` heading becomes an "Overview" chunk. Empty sections are dropped.
 */
export function chunkMdx(source: string, route: string): DocChunk[] {
  const normalized = source.replace(/\r\n?/g, "\n");
  const { title, body } = parseFrontmatter(normalized);
  const lines = body.split("\n");

  const sections: { heading: string; lines: string[] }[] = [
    { heading: "Overview", lines: [] },
  ];
  let inFence = false;
  let fenceMarker = "";
  for (const line of lines) {
    const fenceMatch = line.match(/^\s*(```|~~~)/);
    if (fenceMatch) {
      if (!inFence) {
        inFence = true;
        fenceMarker = fenceMatch[1];
      } else if (fenceMatch[1] === fenceMarker) {
        inFence = false;
      }
      sections[sections.length - 1].lines.push(line);
      continue;
    }
    const h = !inFence && line.match(/^\s*#{2,3}\s+(.+?)\s*#*\s*$/);
    if (h) sections.push({ heading: h[1].trim(), lines: [] });
    else sections[sections.length - 1].lines.push(line);
  }

  const chunks: DocChunk[] = [];
  const usedSlugs = new Map<string, number>();
  for (const s of sections) {
    const text = clean(s.lines.join("\n"));
    if (!text) continue;
    const base = slug(s.heading);
    const count = (usedSlugs.get(base) ?? 0) + 1;
    usedSlugs.set(base, count);
    const idSlug = count === 1 ? base : `${base}-${count}`;
    chunks.push({
      id: `${route}#${idSlug}`,
      route,
      title,
      heading: s.heading,
      text,
    });
  }
  return chunks;
}
