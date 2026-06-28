import { describe, it, expect } from "vitest";
import { parseUpstreamModelList, normalizeBase } from "./provider-import";

describe("parseUpstreamModelList", () => {
  it("reads the OpenAI list shape", () => {
    const out = parseUpstreamModelList({
      object: "list",
      data: [
        { id: "gpt-4o", owned_by: "openai" },
        { id: "gpt-4o-mini" },
      ],
    });
    expect(out).toEqual([
      { id: "gpt-4o", owned_by: "openai" },
      { id: "gpt-4o-mini", owned_by: undefined },
    ]);
  });

  it("reads a bare array", () => {
    expect(parseUpstreamModelList([{ id: "b" }, { id: "a" }])).toEqual([
      { id: "a", owned_by: undefined },
      { id: "b", owned_by: undefined },
    ]);
  });

  it("drops entries with no string id and dedupes", () => {
    const out = parseUpstreamModelList({
      data: [{ id: "x" }, { id: "" }, {}, { id: 7 }, { id: "x" }],
    });
    expect(out).toEqual([{ id: "x", owned_by: undefined }]);
  });

  it("returns [] for garbage", () => {
    expect(parseUpstreamModelList(null)).toEqual([]);
    expect(parseUpstreamModelList("nope")).toEqual([]);
    expect(parseUpstreamModelList({ data: "nope" })).toEqual([]);
  });
});

describe("normalizeBase", () => {
  it("trims and strips trailing slashes", () => {
    expect(normalizeBase("  https://x/v1/  ")).toBe("https://x/v1");
  });
});
