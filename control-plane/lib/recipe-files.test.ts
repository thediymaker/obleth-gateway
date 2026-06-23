import { describe, it, expect } from "vitest";
import { parseRecipeFile } from "./recipe-files";
import {
  buildRecipeCommand,
  buildRecipePreamble,
  recipeDefaults,
} from "./model-recipes";

const GH200 = `
id: llamacpp-gh200
label: llama.cpp (GH200)
badge: Grace-Hopper
hint: native
health_path: /health
image_optional: true
preamble: |
  module load cuda
  module load llama.cpp
command:
  executable: /opt/llama.cpp/bin/llama-server
  model_flag: "-m"
  fixed_args: ["--host", "0.0.0.0"]
  port_flag: "--port"
  alias_flag: "--alias"
params:
  - id: n_cpu_moe
    label: CPU MoE layers
    kind: number
    default: "68"
    arg: { flag: "--n-cpu-moe", omit_when: ["0"] }
  - id: cache_type
    label: KV cache type
    kind: select
    default: q8_0
    arg: { flags: ["--cache-type-k", "--cache-type-v"], omit_when: ["f16"] }
  - id: unified_memory
    label: CUDA unified memory
    kind: boolean
    default: "true"
    arg: { env: GGML_CUDA_ENABLE_UNIFIED_MEMORY, env_value: "1" }
`;

describe("recipe-files loader", () => {
  it("parses a YAML recipe and builds the GH200 command", () => {
    const r = parseRecipeFile(GH200);
    expect(r.id).toBe("llamacpp-gh200");
    expect(r.command?.executable).toBe("/opt/llama.cpp/bin/llama-server");

    const cmd = buildRecipeCommand(r, {
      model: "/m/glm.gguf",
      port: "8000",
      upstreamModel: "glm-5.2",
      values: recipeDefaults(r),
    });
    expect(cmd).toContain("/opt/llama.cpp/bin/llama-server -m /m/glm.gguf");
    expect(cmd).toContain("--host 0.0.0.0 --port 8000");
    expect(cmd).toContain("--alias glm-5.2");
    expect(cmd).toContain("--n-cpu-moe 68");
    expect(cmd).toContain("--cache-type-k q8_0 --cache-type-v q8_0");
  });

  it("emits the recipe preamble (modules + env exports)", () => {
    const r = parseRecipeFile(GH200);
    const preamble = buildRecipePreamble(r, recipeDefaults(r));
    expect(preamble).toContain("module load cuda");
    expect(preamble).toContain("module load llama.cpp");
    expect(preamble).toContain("export GGML_CUDA_ENABLE_UNIFIED_MEMORY=1");
    // module lines come before env exports
    expect(preamble.indexOf("module load cuda")).toBeLessThan(
      preamble.indexOf("export GGML_CUDA_ENABLE_UNIFIED_MEMORY"),
    );
  });

  it("rejects unknown keys (strict schema)", () => {
    expect(() => parseRecipeFile("id: x\nlabel: X\nbogus_key: 1\n")).toThrow();
  });

  it("supports a command_template escape hatch", () => {
    const r = parseRecipeFile(
      `id: ollama\nlabel: Ollama\nhealth_path: /api/tags\ncommand_template: 'serve {{model}} on {{port}}'\n`,
    );
    const cmd = buildRecipeCommand(r, {
      model: "qwen",
      port: "9001",
      upstreamModel: "",
      values: {},
    });
    expect(cmd).toBe("serve qwen on 9001");
  });
});
