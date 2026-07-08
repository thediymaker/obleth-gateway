import { describe, it, expect, vi } from "vitest";

// Keep the tool test independent of real index.json content.
vi.mock("./index.json", () => ({
  default: {
    generatedAt: "2026-07-07T00:00:00Z",
    chunks: [
      { id: "guides/api-keys#rotating", route: "guides/api-keys", title: "API Keys",
        heading: "Rotating a key", text: "To rotate a key, delete the old one and create a new one." },
    ],
  },
}));

import { searchDocsTool } from "./search-docs";
import type { ToolCtx } from "@/lib/charo/tools/types";

const ctx = (): ToolCtx =>
  ({ settings: {} as never, gatewayChat: (() => {}) as never, signal: new AbortController().signal });

describe("search_docs tool", () => {
  it("parseArgs: trims query, defaults + clamps limit, rejects empties", () => {
    expect(searchDocsTool.parseArgs({ query: "  api keys  " })).toEqual({ query: "api keys", limit: 4 });
    expect(searchDocsTool.parseArgs({ query: "x", limit: 99 })).toEqual({ query: "x", limit: 8 });
    expect(searchDocsTool.parseArgs({ query: "x", limit: 0 })).toEqual({ query: "x", limit: 1 });
    expect(() => searchDocsTool.parseArgs({ query: "   " })).toThrow();
    expect(() => searchDocsTool.parseArgs({})).toThrow();
  });

  it("run returns grounded sources from the bundled index", async () => {
    const out = await searchDocsTool.run({ query: "rotate api key", limit: 4 }, ctx(), () => {});
    expect(out.query).toBe("rotate api key");
    expect(out.sources[0]).toMatchObject({ route: "guides/api-keys", heading: "Rotating a key" });
  });

  it("run returns an empty source list when nothing matches", async () => {
    const out = await searchDocsTool.run({ query: "helm airflow", limit: 4 }, ctx(), () => {});
    expect(out.sources).toEqual([]);
  });
});
