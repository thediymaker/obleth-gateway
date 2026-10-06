import { describe, expect, it } from "vitest";
import { ggufQuant, parseRepoId, readHfModel } from "./hf-model";

describe("reading a Hugging Face repo", () => {
  it("reads safetensors weights and suggests vLLM", () => {
    const m = readHfModel("nvidia/Nemotron", {
      siblings: [{ rfilename: "model-00001.safetensors", size: 200e9 }, { rfilename: "model-00002.safetensors", size: 152e9 }, { rfilename: "config.json", size: 1000 }, { rfilename: "chat_template.jinja" }],
      config: { architectures: ["NemotronHForCausalLM"], model_type: "nemotron_h", quantization_config: { quant_method: "modelopt", quant_algo: "NVFP4" } },
      cardData: { license: "other" },
    });
    expect(m).toMatchObject({ format: "safetensors", weightsGb: 352, quantization: "modelopt (NVFP4)", chatTemplate: true, recommended: "vllm" });
    expect(m.engines.map((e) => [e.engine, e.ok])).toEqual([["vllm", true], ["sglang", true], ["llamacpp", false], ["ollama", false]]);
  });

  it("lists GGUF quantizations and suggests llama.cpp", () => {
    const m = readHfModel("unsloth/GLM-5.2-GGUF", { siblings: [{ rfilename: "UD-IQ2_M/GLM-5.2-UD-IQ2_M-00001-of-00002.gguf", size: 100e9 }, { rfilename: "UD-IQ2_M/GLM-5.2-UD-IQ2_M-00002-of-00002.gguf", size: 90e9 }, { rfilename: "GLM-5.2-Q4_K_M.gguf", lfs: { size: 380e9 } }] });
    expect(m.ggufQuants).toEqual([{ name: "UD-IQ2_M", gb: 190 }, { name: "Q4_K_M", gb: 380 }]);
    expect(m.engines.find((e) => e.engine === "llamacpp")?.model).toBe("unsloth/GLM-5.2-GGUF:Q4_K_M");
    expect(m.engines.find((e) => e.engine === "ollama")?.model).toBe("hf.co/unsloth/GLM-5.2-GGUF:Q4_K_M");
    expect(m.recommended).toBe("llamacpp");
  });

  it("finds the quantization in a file name", () => {
    expect(ggufQuant("Qwen3-32B-Q8_0.gguf")).toBe("Q8_0");
    expect(ggufQuant("model-BF16-00001-of-00003.gguf")).toBe("BF16");
    expect(ggufQuant("README.md")).toBeNull();
  });

  it("accepts ids and links", () => {
    expect(parseRepoId("https://huggingface.co/deepseek-ai/DeepSeek-V4.1-Flash/tree/main")).toBe("deepseek-ai/DeepSeek-V4.1-Flash");
    expect(parseRepoId(" Qwen/Qwen3-32B ")).toBe("Qwen/Qwen3-32B");
    expect(parseRepoId("not a model")).toBeNull();
  });
});
