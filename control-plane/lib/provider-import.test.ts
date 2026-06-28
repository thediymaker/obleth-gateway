import { describe, it, expect } from "vitest";
import { parseUpstreamModelList, normalizeBase, classifyDiscovered } from "./provider-import";

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

describe("classifyDiscovered", () => {
  const existing = [
    { model_name: "gpt-4o", upstream_model: "gpt-4o", api_base: "https://api.openai.com/v1" },
    { model_name: "my-llama", upstream_model: "meta/llama", api_base: "https://x/v1" },
  ];

  it("flags a name collision as existing", () => {
    const rows = classifyDiscovered([{ id: "GPT-4o" }], existing, "https://other/v1");
    expect(rows[0]).toEqual({
      id: "GPT-4o",
      modelName: "gpt-4o",
      ownedBy: undefined,
      status: "existing",
    });
  });

  it("flags a same base + upstream pair as existing even when name differs", () => {
    const rows = classifyDiscovered([{ id: "meta/llama" }], existing, "https://x/v1/");
    expect(rows[0].status).toBe("existing");
  });

  it("marks genuinely new models as new", () => {
    const rows = classifyDiscovered([{ id: "claude-x" }], existing, "https://x/v1");
    expect(rows[0]).toEqual({
      id: "claude-x",
      modelName: "claude-x",
      ownedBy: undefined,
      status: "new",
    });
  });
});
