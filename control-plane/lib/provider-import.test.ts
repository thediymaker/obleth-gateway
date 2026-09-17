import { describe, it, expect } from "vitest";
import { parseUpstreamModelList, normalizeBase, classifyDiscovered, buildImportPayload, splitQuantizationSuffix, type BatchDefaults, type RowState } from "./provider-import";

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
      quantization: "unknown",
      suggestedAlias: undefined,
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
      quantization: "unknown",
      suggestedAlias: undefined,
    });
  });

  it("lifts a quantization suffix out of the suggested name and offers it as an alias", () => {
    const rows = classifyDiscovered([{ id: "Qwen/Qwen3-8B-FP8" }], existing, "https://x/v1");
    expect(rows[0]).toEqual({
      id: "Qwen/Qwen3-8B-FP8",
      modelName: "qwen-qwen3-8b",
      ownedBy: undefined,
      status: "new",
      quantization: "fp8",
      // The provider's own id keeps resolving through the gateway.
      suggestedAlias: "qwen-qwen3-8b-fp8",
    });
  });

  it("does not re-offer a route that was registered under the full suffixed name", () => {
    const registered = [
      { model_name: "glm-5-3-mxfp4", upstream_model: "glm-5-3-mxfp4", api_base: "https://y/v1" },
    ];
    const rows = classifyDiscovered([{ id: "glm-5-3-mxfp4" }], registered, "https://x/v1");
    // The clean suggestion is `glm-5-3`, which matches nothing — but the model
    // is already registered under the name the suffix split away.
    expect(rows[0].modelName).toBe("glm-5-3");
    expect(rows[0].status).toBe("existing");
  });
});

describe("splitQuantizationSuffix", () => {
  it("recognizes the formats providers spell into their ids", () => {
    expect(splitQuantizationSuffix("qwen3-8b-fp8")).toEqual({ name: "qwen3-8b", quantization: "fp8" });
    expect(splitQuantizationSuffix("glm-5-3-mxfp4")).toEqual({ name: "glm-5-3", quantization: "mxfp4" });
    expect(splitQuantizationSuffix("llama-70b-awq")).toEqual({ name: "llama-70b", quantization: "awq" });
    // Dot is a legal separator in an obleth model name.
    expect(splitQuantizationSuffix("gemma3.27b.bf16")).toEqual({ name: "gemma3.27b", quantization: "bf16" });
  });

  it("does not read the 4-bit float formats as each other", () => {
    expect(splitQuantizationSuffix("m-nvfp4").quantization).toBe("nvfp4");
    expect(splitQuantizationSuffix("m-mxfp4").quantization).toBe("mxfp4");
  });

  it("leaves a name alone when there is no suffix, or nothing left without it", () => {
    expect(splitQuantizationSuffix("qwen3-8b")).toEqual({ name: "qwen3-8b", quantization: "unknown" });
    // A model genuinely called `fp8` keeps its name rather than becoming "".
    expect(splitQuantizationSuffix("fp8")).toEqual({ name: "fp8", quantization: "unknown" });
    // Only a trailing token is lifted; mid-name text is part of the name.
    expect(splitQuantizationSuffix("fp8-tuned")).toEqual({ name: "fp8-tuned", quantization: "unknown" });
  });
});

const DEFAULTS: BatchDefaults = {
  model_type: "chat",
  context_window: 8192,
  input_cost_per_token: 0,
  enabled: true,
};

function row(over: Partial<RowState> = {}): RowState {
  return { id: "gpt-4o", modelName: "gpt-4o", included: true, overrides: {}, quantization: "unknown", aliases: [], ...over };
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

  it("carries a lifted quantization and its alias into the manifest entry", () => {
    const out = buildImportPayload(
      [row({ modelName: "qwen3-8b", quantization: "fp8", aliases: ["qwen3-8b-fp8"] })],
      "https://x/v1",
      undefined,
      DEFAULTS,
    );
    expect(out.models[0].quantization).toBe("fp8");
    expect(out.models[0].aliases).toEqual(["qwen3-8b-fp8"]);
  });

  it("omits an undeclared format and an empty alias list", () => {
    const out = buildImportPayload([row()], "https://x/v1", undefined, DEFAULTS);
    expect(out.models[0]).not.toHaveProperty("quantization");
    expect(out.models[0]).not.toHaveProperty("aliases");
  });

  it("normalizes an un-blurred modelName at payload build time", () => {
    const out = buildImportPayload([row({ modelName: "Foo Bar" })], "https://x/v1", undefined, DEFAULTS);
    expect(out.models[0].model_name).toBe("foo-bar");
  });
});
