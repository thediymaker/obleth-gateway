import { describe, it, expect } from "vitest";
import { searchDocs } from "./search";
import type { DocsIndex } from "./types";

const index: DocsIndex = {
  generatedAt: "2026-07-07T00:00:00Z",
  chunks: [
    { id: "guides/api-keys#overview", route: "guides/api-keys", title: "API Keys",
      heading: "Overview", text: "API keys authenticate clients against the data plane." },
    { id: "guides/api-keys#rotating", route: "guides/api-keys", title: "API Keys",
      heading: "Rotating a key", text: "To rotate your keys, delete the old keys and create new ones." },
    { id: "guides/quotas#overview", route: "guides/quotas", title: "Quotas",
      heading: "Overview", text: "Quotas cap tokens and cost per tenant over a period." },
  ],
};

describe("searchDocs", () => {
  it("ranks the most relevant chunk first", () => {
    const out = searchDocs(index, "how do I rotate an api key", 3);
    expect(out[0].heading).toBe("Rotating a key");
  });

  it("boosts matches in the heading/title", () => {
    const out = searchDocs(index, "quotas", 3);
    expect(out[0].route).toBe("guides/quotas");
  });

  it("respects the limit (truncates multi-match results)", () => {
    // "key keys" matches both the overview ("keys") and rotating ("key") chunks.
    expect(searchDocs(index, "key keys", 5).length).toBe(2);
    expect(searchDocs(index, "key keys", 1).length).toBe(1);
  });

  it("returns [] for an empty or all-stopword query", () => {
    expect(searchDocs(index, "   ", 3)).toEqual([]);
    expect(searchDocs(index, "the a of", 3)).toEqual([]);
  });

  it("returns [] when nothing matches", () => {
    expect(searchDocs(index, "kubernetes helm airflow", 3)).toEqual([]);
  });

  it("produces a non-empty snippet for each source", () => {
    const out = searchDocs(index, "rotate key", 3);
    expect(out[0].snippet.length).toBeGreaterThan(0);
  });

  it("is safe on an empty index", () => {
    expect(searchDocs({ generatedAt: "", chunks: [] }, "anything", 3)).toEqual([]);
  });

  it("flattens markdown tables out of snippets", () => {
    const index: DocsIndex = {
      generatedAt: "t",
      chunks: [
        {
          id: "guides/schema#tables",
          route: "guides/schema",
          title: "Schema",
          heading: "Tables",
          text:
            "Migrations run in order:\n" +
            "| File | What it adds |\n" +
            "| --- | --- |\n" +
            "| init.sql | Full initial schema tracing tenant |",
        },
      ],
    };
    const [s] = searchDocs(index, "tracing tenant", 4);
    expect(s.snippet).not.toContain("|");
    expect(s.snippet).not.toMatch(/---/);
    expect(s.snippet).toContain("·"); // cell divider middot
  });

  it("snaps snippet bounds to whole words", () => {
    const head = Array.from({ length: 60 }, (_, i) => `word${i}`).join(" ");
    const tail = Array.from({ length: 60 }, (_, i) => `tail${i}`).join(" ");
    const index: DocsIndex = {
      generatedAt: "t",
      chunks: [
        {
          id: "a#x",
          route: "a",
          title: "A",
          heading: "H",
          text: `${head} targetterm ${tail}`,
        },
      ],
    };
    const known = new Set([
      ...Array.from({ length: 60 }, (_, i) => `word${i}`),
      "targetterm",
      ...Array.from({ length: 60 }, (_, i) => `tail${i}`),
    ]);
    const [s] = searchDocs(index, "targetterm", 4);
    const body = s.snippet.replace(/^\.\.\./, "").replace(/\.\.\.$/, "").trim();
    const toks = body.split(" ");
    expect(known.has(toks[0])).toBe(true);
    expect(known.has(toks[toks.length - 1])).toBe(true);
  });

  it("leaves shell and regex pipes intact", () => {
    const index: DocsIndex = {
      generatedAt: "t",
      chunks: [
        {
          id: "guides/cli#pipes",
          route: "guides/cli",
          title: "CLI",
          heading: "Filtering",
          text:
            "To filter tracing output run cat obleth.log | grep tenant here.\n" +
            "You can also match either token with the regex a|b in your query.",
        },
      ],
    };
    const [s] = searchDocs(index, "tracing tenant", 4);
    expect(s.snippet).toContain("cat obleth.log | grep tenant");
    expect(s.snippet).not.toContain("·");
  });

  it("still flattens a genuine table while leaving a fenced pipe alone", () => {
    const index: DocsIndex = {
      generatedAt: "t",
      chunks: [
        {
          id: "guides/schema#tables",
          route: "guides/schema",
          title: "Schema",
          heading: "Tables",
          text:
            "Tracing tenant fields:\n" +
            "| Field | Purpose |\n" +
            "| --- | --- |\n" +
            "| tenant_id | owner |",
        },
      ],
    };
    const [s] = searchDocs(index, "tracing tenant", 4);
    expect(s.snippet).not.toContain("|");
    expect(s.snippet).toContain("·");
  });

  it("leaves fenced-code pipes intact while flattening an adjacent table", () => {
    const index: DocsIndex = {
      generatedAt: "t",
      chunks: [
        {
          id: "guides/x#f",
          route: "guides/x",
          title: "X",
          heading: "Config",
          text:
            "Tracing tenant config below.\n" +
            "```bash\ncat obleth.log | grep tenant\n```\n" +
            "| Field | Purpose |\n| --- | --- |\n| tenant_id | owner |",
        },
      ],
    };
    const [s] = searchDocs(index, "tracing tenant", 4);
    expect(s.snippet).toContain("cat obleth.log | grep tenant"); // fenced pipe intact
    expect(s.snippet).not.toContain("| Field |");                // table flattened
    expect(s.snippet).toContain("·");
  });

  it("leaves a prose pipe abutting a table intact", () => {
    const index: DocsIndex = {
      generatedAt: "t",
      chunks: [
        {
          id: "guides/y#a",
          route: "guides/y",
          title: "Y",
          heading: "Fields",
          text:
            "Filter tracing with cat log | grep tenant first.\n" +
            "| Field | Purpose |\n| --- | --- |\n| tenant_id | owner |",
        },
      ],
    };
    const [s] = searchDocs(index, "tracing tenant", 4);
    expect(s.snippet).toContain("cat log | grep tenant"); // prose pipe intact
    expect(s.snippet).not.toContain("| Field |");         // table still flattened
  });

  it("drops matches far below the top score", () => {
    const index: DocsIndex = {
      generatedAt: "t",
      chunks: [
        {
          id: "guides/api-keys#rotate",
          route: "guides/api-keys",
          title: "API Keys",
          heading: "rotate api keys",
          text: "rotate api keys regularly to limit blast radius",
        },
        {
          id: "guides/tenants#overview",
          route: "guides/tenants",
          title: "Tenants",
          heading: "Overview",
          text: "tenants can rotate their own settings",
        },
      ],
    };
    const results = searchDocs(index, "rotate api keys", 4);
    expect(results).toHaveLength(1);
    expect(results[0].route).toBe("guides/api-keys");
  });

  it("keeps comparably-scoring sources", () => {
    const index: DocsIndex = {
      generatedAt: "t",
      chunks: [
        {
          id: "a#rotate",
          route: "a",
          title: "A",
          heading: "rotate api keys",
          text: "rotate api keys here",
        },
        {
          id: "b#rotate",
          route: "b",
          title: "B",
          heading: "rotate api keys often",
          text: "rotate api keys frequently",
        },
      ],
    };
    const results = searchDocs(index, "rotate api keys", 4);
    expect(results).toHaveLength(2);
  });
});
