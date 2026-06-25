// Pure (no fs) splitter for `---`-fenced recipe documents, shared by the
// server-side loader (sbatch-recipes.ts) and the client recipe gallery
// (recipes/recipe-list.tsx). Kept in its own module so the client can import it
// without pulling in the fs-touching loader.

/** Split a Jekyll-style `---`\n header \n`---`\n body document. Returns null
 *  when the opening/closing fence is missing. */
export function splitFrontmatter(text: string): { header: string; body: string } | null {
  const norm = text.replace(/\r\n/g, "\n");
  if (!norm.startsWith("---\n")) return null;
  // Match a closing fence: exactly "---" on its own line.
  const m = norm.slice(4).match(/\n---(?:\r?\n|$)/);
  if (!m || m.index === undefined) return null;
  const fenceStart = 4 + m.index; // index of the "\n" before "---"
  const header = norm.slice(4, fenceStart);
  const body = norm.slice(fenceStart + m[0].length); // skip past "\n---\n"
  return { header, body };
}
