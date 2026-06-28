import { describe, it, expect } from "vitest";
import { parseUpstreamModelList, normalizeBase, classifyDiscovered, buildImportPayload, type BatchDefaults, type RowState } from "./provider-import";

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

const DEFAULTS: BatchDefaults = {
  model_type: "chat",
  context_window: 8192,
  input_cost_per_token: 0,
  enabled: true,
};

function row(over: Partial<RowState> = {}): RowState {
  return { id: "gpt-4o", modelName: "gpt-4o", included: true, overrides: {}, ...over };
}

describe("buildImportPayload", () => {
  it("excludes unselected rows", () => {
    const out = buildImportPayload([row({ included: false })], "https://x/v1", undefined, DEFAULTS);
    expect(out.models).toEqual([]);
  });

  it("wires base, upstream id, key, and batch defaults", () => {
    const out = buildImportPayload([row()], "https://x/v1/", "sk-1", DEFAULTS);
    expect(out.models[0]).toMatchObject({
      model_name: "gpt-4o",
      upstream_model: "gpt-4o",
      api_base: "https://x/v1",
      api_key: "sk-1",
      model_type: "chat",
      context_window: 8192,
      enabled: true,
    });
  });

  it("omits api_key when none is given", () => {
    const out = buildImportPayload([row()], "https://x/v1", undefined, DEFAULTS);
    expect(out.models[0]).not.toHaveProperty("api_key");
  });

  it("lets a per-row override win over the batch default", () => {
    const out = buildImportPayload(
      [row({ overrides: { model_type: "embedding", context_window: 512 } })],
      "https://x/v1",
      undefined,
      DEFAULTS,
    );
    expect(out.models[0]).toMatchObject({ model_type: "embedding", context_window: 512 });
  });

  it("emits the three fields the importer requires for every row", () => {
    const out = buildImportPayload([row()], "https://x/v1", undefined, DEFAULTS);
    for (const m of out.models) {
      expect(m.model_name).toBeTruthy();
      expect(m.upstream_model).toBeTruthy();
      expect(m.api_base).toBeTruthy();
    }
  });
});
