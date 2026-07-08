/** One heading-level section of a docs page. */
export interface DocChunk {
  /** `${route}#${slug(heading)}` - stable, unique. */
  id: string;
  /** Docs-site route, e.g. "guides/api-keys". */
  route: string;
  /** Page title from frontmatter. */
  title: string;
  /** Section heading, or "Overview" for pre-heading content. */
  heading: string;
  /**
   * Plaintext section body: `<img>` tags and stray JSX/HTML tags in prose are
   * stripped, while fenced code blocks and inline code spans (including any
   * angle-bracket placeholders they contain, e.g. `<OBLETH_ADMIN_TOKEN>`) are
   * preserved verbatim.
   */
  text: string;
}

/** The bundled, checked-in search index. */
export interface DocsIndex {
  generatedAt: string;
  chunks: DocChunk[];
}

/** A single cited source returned to the brain / rendered in the card. */
export interface DocsSource {
  route: string;
  title: string;
  heading: string;
  snippet: string;
}

/** `search_docs` tool result payload (resultType "docs_result"). */
export interface DocsSearchResult {
  query: string;
  sources: DocsSource[];
}
