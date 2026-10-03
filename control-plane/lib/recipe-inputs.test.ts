import { describe, expect, it } from "vitest";
import { EMPTY_CLUSTER, inputDefault, inputProblem, inputSource, missingClusterValues, renderScript, resolveClusterTokens, type ClusterValues, type RecipeInput } from "./recipe-inputs";

const cv: ClusterValues = { cache: "/scratch/hf", images: "/scratch/images", logs: "/scratch/logs", setup: "module load apptainer", image: { vllm: "vllm.sif", sglang: "/opt/sglang.sif" } };

describe("cluster values", () => {
  it("fills paths, joining image names onto the images folder", () => {
    expect(resolveClusterTokens("{{cluster.cache}} {{cluster.image.vllm}} {{cluster.image.sglang}}", cv)).toBe("/scratch/hf /scratch/images/vllm.sif /opt/sglang.sif");
  });

  it("leaves unset values in place and names them", () => {
    expect(resolveClusterTokens("{{cluster.image.ollama}}", cv)).toBe("{{cluster.image.ollama}}");
    expect(missingClusterValues("{{cluster.image.ollama}} {{cluster.cache}}", cv)).toEqual(["cluster.image.ollama"]);
  });

  it("treats unset setup lines as nothing to run", () => {
    expect(resolveClusterTokens("a\n{{cluster.setup}}\nb", EMPTY_CLUSTER)).toBe("a\n\nb");
  });
});

describe("inputs", () => {
  const offload: RecipeInput = { name: "offload_gb", type: "number", default: "270", min: 0, max: 460, required: false, by_nodes: { "4": "10", "8": "0" } };
  const image: RecipeInput = { name: "image", type: "path", default: "{{cluster.image.vllm}}", required: true };

  it("defaults follow the node count", () => {
    expect(inputDefault(offload, cv, 1)).toBe("270");
    expect(inputDefault(offload, cv, 4)).toBe("10");
    expect(inputDefault(offload, cv)).toBe("270");
  });

  it("says where a default comes from", () => {
    expect(inputSource(image)).toBe("cluster");
    expect(inputSource(offload)).toBe("recipe");
  });

  it("starts an optional input empty when its cluster value isn't set", () => {
    expect(inputDefault({ name: "image", type: "path", default: "{{cluster.image.llamacpp}}", required: false }, cv)).toBe("");
    expect(inputDefault({ ...image, default: "{{cluster.image.llamacpp}}" }, cv)).toBe("{{cluster.image.llamacpp}}");
  });

  it("checks numbers, choices and missing cluster values", () => {
    expect(inputProblem(offload, "500")).toMatch(/at most 460/);
    expect(inputProblem(offload, "abc")).toMatch(/number/);
    expect(inputProblem({ name: "context", type: "choice", options: ["65536", "131072"], required: false }, "1")).toMatch(/one of/);
    expect(inputProblem(image, "{{cluster.image.ollama}}")).toMatch(/cluster default/);
    expect(inputProblem(image, "")).toMatch(/needed/);
    expect(inputProblem(image, "/x.sif")).toBeNull();
  });
});

describe("renderScript", () => {
  const inputs: RecipeInput[] = [
    { name: "image", type: "path", default: "{{cluster.image.vllm}}", required: true },
    { name: "tools", type: "flag", default: "true", adds: "--enable-auto-tool-choice", required: false },
    { name: "mtp", type: "flag", default: "false", adds: "--speculative", required: false },
  ];
  const body = "{{cluster.setup}}\nrun {{image}} --name {{api_model_name}} {{tools}}{{mtp}} ${HOME}";

  it("fills inputs, flags, built-ins and cluster values", () => {
    expect(renderScript(body, inputs, { mtp: "true" }, cv, { builtins: { api_model_name: "m" } })).toBe("module load apptainer\nrun /scratch/images/vllm.sif --name m --enable-auto-tool-choice--speculative ${HOME}");
  });

  it("drops a flag that is off", () => {
    expect(renderScript("x {{tools}}", inputs, { tools: "false" }, cv)).toBe("x ");
  });

  it("leaves nothing behind for an optional input with no value", () => {
    const opt: RecipeInput[] = [
      { name: "context", type: "number", required: false },
      { name: "extra", type: "text", required: false },
    ];
    const script = 'CONTEXT="{{context}}"\nvllm serve m {{extra}}';
    expect(renderScript(script, opt, {}, cv, { strict: true })).toBe('CONTEXT=""\nvllm serve m ');
    expect(renderScript(script, opt, { context: " ", extra: "" }, cv)).toBe('CONTEXT=""\nvllm serve m ');
  });

  it("keeps a required input's token for the review until it has a value", () => {
    const model: RecipeInput[] = [{ name: "model", type: "text", required: true }];
    expect(renderScript("serve {{model}}", model, {}, cv)).toBe("serve {{model}}");
    expect(() => renderScript("serve {{model}}", model, {}, cv, { strict: true })).toThrow(/model/);
  });

  it("refuses to launch with a cluster default missing", () => {
    expect(() => renderScript("run {{image}}", inputs, {}, EMPTY_CLUSTER, { strict: true })).toThrow(/cluster.image.vllm isn't set/);
  });
});
