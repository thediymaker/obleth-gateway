/**
 * Make a raw docs-chunk snippet presentable in the sources list: strip
 * markdown syntax, collapse table plumbing, and trim mid-word cut-offs so a
 * snippet never opens with a token fragment like "…remental".
 */
export function cleanSnippet(raw: string): string {
  let s = (raw ?? "").trim();

  // Leading ellipsis: drop it, and drop a partial first token unless the
  // remainder starts cleanly (capital letter, digit-as-version is rare in
  // docs prose, so capitals are the signal).
  if (/^(…|\.\.\.)/.test(s)) {
    s = s.replace(/^(…|\.\.\.)\s*/, "");
    if (/^[a-z]/.test(s)) s = s.replace(/^\S+\s*/, "");
  }

  // Markdown table separator cells (| --- |, | :--- |) vanish entirely.
  s = s.replace(/\|\s*:?-{2,}:?\s*(?=\||$)/g, "|");
  s = s.replace(/\|{2,}/g, "|");
  // Remaining pipes read as interpunct-separated fragments.
  s = s.replace(/\s*\|\s*/g, " · ");
  s = s.replace(/(?:\s*·\s*){2,}/g, " · ");

  // Emphasis, code spans, headings: keep the content, lose the syntax.
  s = s.replace(/\*\*([^*]+)\*\*/g, "$1");
  s = s.replace(/\*([^*]+)\*/g, "$1");
  s = s.replace(/__([^_]+)__/g, "$1");
  s = s.replace(/`([^`]*)`/g, "$1");
  s = s.replace(/^#+\s+/gm, "");

  s = s.replace(/\s+/g, " ").trim();
  s = s.replace(/^(?:·\s*)+/, "").replace(/(?:\s*·)+$/, "").trim();
  return s;
}
