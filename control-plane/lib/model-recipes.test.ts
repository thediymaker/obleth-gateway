import { describe, it, expect } from "vitest";
import {
  SLURM_RECIPES,
  recipeDefaults,
  buildRecipeCommand,
  buildRecipePreamble,
  recommendNCpuMoe,
  type SlurmRecipe,
} from "./model-recipes";

function recipe(id: string): SlurmRecipe {
  const r = SLURM_RECIPES.find((x) => x.id === id);
  if (!r) throw new Error(`no recipe ${id}`);
  return r;
}

function build(id: string, overrides: Record<string, string> = {}, ctx: { model?: string; port?: string; upstreamModel?: string } = {}) {
  const r = recipe(id);
  return buildRecipeCommand(r, {
    model: ctx.model ?? "",
    port: ctx.port ?? "8000",
    upstreamModel: ctx.upstreamModel ?? "",
    values: { ...recipeDefaults(r), ...overrides },
  });
}

describe("llama.cpp recipe", () => {
  it("emits defaults without optional flags", () => {
    const cmd = build("llamacpp", {}, { model: "/m/model.gguf", upstreamModel: "glm-5.2" });
    expect(cmd).toContain("llama-server -m /m/model.gguf");
    expect(cmd).toContain("--host 0.0.0.0 --port 8000");
    expect(cmd).toContain("--alias glm-5.2");
    expect(cmd).toContain("-ngl 99");
    expect(cmd).toContain("--ctx-size 32768");
    expect(cmd).toContain("--jinja");
    expect(cmd).toContain("--flash-attn on");
    // off by default
    expect(cmd).not.toContain("--n-cpu-moe");
    expect(cmd).not.toContain("--cache-type-k");
    expect(cmd).not.toContain("--no-mmap");
  });

  it("includes the GH200 knobs when set", () => {
    const cmd = build(
      "llamacpp",
      { n_cpu_moe: "68", cache_type: "q8_0", ctx_size: "1048576", no_mmap: "true", parallel: "2" },
      { model: "/m/model.gguf", upstreamModel: "glm-5.2" },
    );
    expect(cmd).toContain("--n-cpu-moe 68");
    expect(cmd).toContain("--ctx-size 1048576");
    expect(cmd).toContain("--cache-type-k q8_0 --cache-type-v q8_0");
    expect(cmd).toContain("--parallel 2");
    expect(cmd).toContain("--no-mmap");
  });

  it("drops disabled boolean flags", () => {
    const cmd = build("llamacpp", { flash_attn: "false", jinja: "false" }, { model: "/m/m.gguf" });
    expect(cmd).not.toContain("--flash-attn");
    expect(cmd).not.toContain("--jinja");
  });

  it("exports unified memory via the preamble only when enabled", () => {
    const r = recipe("llamacpp");
    expect(buildRecipePreamble(r, { ...recipeDefaults(r), unified_memory: "true" })).toBe(
      "export GGML_CUDA_ENABLE_UNIFIED_MEMORY=1",
    );
    expect(buildRecipePreamble(r, { ...recipeDefaults(r) })).toBe("");
  });
});

describe("vLLM recipe", () => {
  it("adds served-model-name only when it differs from the model", () => {
    const same = build("vllm", {}, { model: "Qwen/Qwen3-8B", upstreamModel: "Qwen/Qwen3-8B" });
    expect(same).not.toContain("--served-model-name");

    const diff = build("vllm", {}, { model: "Qwen/Qwen3-8B", upstreamModel: "qwen3" });
    expect(diff).toContain("--served-model-name qwen3");
  });

  it("emits tuning flags only when non-default", () => {
    const cmd = build("vllm", { tensor_parallel: "4", max_model_len: "8192", dtype: "bfloat16" }, { model: "m" });
    expect(cmd).toContain("--tensor-parallel-size 4");
    expect(cmd).toContain("--max-model-len 8192");
    expect(cmd).toContain("--dtype bfloat16");

    const bare = build("vllm", {}, { model: "m" });
    expect(bare).not.toContain("--tensor-parallel-size");
    expect(bare).not.toContain("--dtype");
  });
});

describe("ollama recipe", () => {
  it("serves and pulls the tag on the given port", () => {
    const cmd = build("ollama", {}, { model: "qwen2.5:0.5b", port: "9001" });
    expect(cmd).toContain("OLLAMA_HOST=0.0.0.0:9001 ollama serve");
    expect(cmd).toContain("ollama pull qwen2.5:0.5b");
  });
});

describe("recommendNCpuMoe", () => {
  it("rises with context length on a ~96GB GH200", () => {
    const small = recommendNCpuMoe(16_384, 96)!;
    const big = recommendNCpuMoe(1_048_576, 96)!;
    expect(small).toBeGreaterThan(0);
    expect(big).toBeGreaterThan(small); // more ctx => more offload
  });
  it("returns null when VRAM is unknown (no honest guess)", () => {
    expect(recommendNCpuMoe(1_048_576, null)).toBeNull();
  });
});
