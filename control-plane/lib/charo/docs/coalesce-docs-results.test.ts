import { describe, it, expect } from "vitest";
import { coalesceDocsResults } from "./coalesce-docs-results";
import type { DocsSearchResult } from "./types";

const src = (route: string, heading: string) => ({
  route,
  title: route,
  heading,
  snippet: "…",
});

const docs = (query: string, sources: ReturnType<typeof src>[]) => ({
  type: "docs_result",
  data: { query, sources } satisfies DocsSearchResult,
});

describe("coalesceDocsResults", () => {
  it("passes through when there is at most one docs_result", () => {
    const one = [docs("q", [src("a", "h")]), { type: "bench_result", data: {} }];
    expect(coalesceDocsResults(one)).toEqual(one);
    expect(coalesceDocsResults([])).toEqual([]);
  });

  it("merges multiple docs_result entries into one, deduped by route+heading", () => {
    const out = coalesceDocsResults([
      docs("tenant tracing", [src("observability", "Enabling tracing"), src("schema", "Overview")]),
      docs("api key tracing", [src("observability", "Enabling tracing"), src("schema", "api_keys")]),
    ]);
    expect(out).toHaveLength(1);
    const merged = out[0].data as DocsSearchResult;
    expect(merged.sources.map((s) => `${s.route}#${s.heading}`)).toEqual([
      "observability#Enabling tracing",
      "schema#Overview",
      "schema#api_keys",
    ]);
  });

  it("keeps the merged entry at the first docs_result's position and other entries intact", () => {
    const out = coalesceDocsResults([
      { type: "capability_result", data: {} },
      docs("q1", [src("a", "h1")]),
      { type: "tool_error", data: { message: "x" } },
      docs("q2", [src("b", "h2")]),
    ]);
    expect(out.map((r) => r.type)).toEqual(["capability_result", "docs_result", "tool_error"]);
    expect((out[1].data as DocsSearchResult).sources).toHaveLength(2);
  });

  it("keeps the first entry's query and tolerates malformed data", () => {
    const out = coalesceDocsResults([
      docs("first query", [src("a", "h")]),
      { type: "docs_result", data: null },
      { type: "docs_result", data: { sources: "not-an-array" } },
    ]);
    expect(out).toHaveLength(1);
    const merged = out[0].data as DocsSearchResult;
    expect(merged.query).toBe("first query");
    expect(merged.sources).toHaveLength(1);
  });
});
