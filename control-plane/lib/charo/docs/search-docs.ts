import type { CharoTool } from "@/lib/charo/tools/types";
import type { DocsIndex, DocsSearchResult } from "./types";
import { searchDocs } from "./search";
import indexJson from "./index.json";

const INDEX = indexJson as DocsIndex;

interface Args {
  query: string;
  limit: number;
}

export const searchDocsTool: CharoTool<Args, DocsSearchResult> = {
  name: "search_docs",
  description:
    "Search the official obleth documentation and return the most relevant sections. " +
    "Use this for any how-to, setup, or configuration question about obleth (API keys, " +
    "tenants, quotas, routing, deployment, etc.). Cite the pages you use in your answer, " +
    "and say plainly when the docs don't cover something rather than guessing.",
  parameters: {
    type: "object",
    properties: {
      query: { type: "string", description: "What to look up in the docs." },
      limit: { type: "number", description: "Max sections to return (default 4).", minimum: 1, maximum: 8 },
    },
    required: ["query"],
    additionalProperties: false,
  },
  resultType: "docs_result",
  requiresConfirmation: false, // reads a static bundled file - read-only

  parseArgs(raw: unknown): Args {
    const o = (raw ?? {}) as Record<string, unknown>;
    const query = typeof o.query === "string" ? o.query.trim() : "";
    if (!query) throw new Error("`query` is required.");
    let limit = typeof o.limit === "number" && Number.isFinite(o.limit) ? Math.floor(o.limit) : 4;
    limit = Math.max(1, Math.min(8, limit));
    return { query, limit };
  },

  async run(args): Promise<DocsSearchResult> {
    return { query: args.query, sources: searchDocs(INDEX, args.query, args.limit) };
  },
};
