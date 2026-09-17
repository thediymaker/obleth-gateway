import { existsSync } from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { ALL_PROVIDERS, providerForModel } from "./model-providers";

// name -> expected provider id. One entry per provider in the table, using the
// model names those vendors actually publish.
const CASES: Array<[string, string]> = [
  ["Qwen3-235B-A22B-Instruct", "qwen"],
  ["gte-modernbert-base", "qwen"],
  ["gemma-3-27b-it", "google"],
  ["embeddinggemma-300m", "google"],
  ["Llama-3.3-70B-Instruct", "meta"],
  ["muse-glimmer-30b", "meta"],
  ["MiniMax-M2", "minimax"],
  ["Mistral-Small-3.2-24B-Instruct", "mistral"],
  ["gpt-oss-120b", "openai"],
  ["text-embedding-3-small", "openai"],
  ["GLM-4.6", "zai"],
  ["granite-4.0-h-small", "ibm"],
  ["DeepSeek-V3.2", "deepseek"],
  ["Kimi-K2-Instruct", "moonshot"],
  ["Phi-4-reasoning", "microsoft"],
  ["multilingual-e5-large", "microsoft"],
  ["Nemotron-H-56B-Base", "nvidia"],
  ["nv-embed-v2", "nvidia"],
  ["internlm3-8b-instruct", "internlm"],
  ["Hunyuan-A13B-Instruct", "tencent"],
  ["ERNIE-4.5-300B-A47B", "baidu"],
  ["Seed-OSS-36B-Instruct", "bytedance"],
  ["step-3", "stepfun"],
  ["MiMo-VL-7B-RL", "xiaomi"],
  ["Ling-1T", "inclusionai"],
  ["MiniCPM4.1-8B", "openbmb"],
  ["Mellum-4b-base", "jetbrains"],
  ["command-a-03-2025", "cohere"],
  ["aya-expanse-32b", "cohere"],
  ["embed-v4.0", "cohere"],
  ["Inkling-Small", "thinkingmachines"],
  ["Laguna-S-2.1", "poolside"],
  ["Ornith-1.5-9B", "ornith"],
  ["FLUX.1-dev", "blackforestlabs"],
  ["grok-4", "xai"],
  ["claude-sonnet-5", "anthropic"],
  ["Jamba-Large-1.7", "ai21"],
  ["Falcon3-10B-Instruct", "tii"],
  ["OLMo-2-1124-13B-Instruct", "ai2"],
  ["LFM2-1.2B", "liquid"],
  ["Hermes-4-70B", "nous"],
  ["stable-diffusion-3.5-large", "stability"],
  ["Yi-1.5-34B-Chat", "01ai"],
  ["Baichuan-M2-32B", "baichuan"],
  ["solar-pro-2", "upstage"],
  ["EXAONE-4.0-32B", "lg"],
  ["snowflake-arctic-embed-l-v2.0", "snowflake"],
  ["us.amazon.nova-pro-v1:0", "amazon"],
  ["sonar-reasoning-pro", "perplexity"],
  ["Skywork-R1V3-38B", "skywork"],
  ["openPangu-Embedded-7B", "huawei"],
  ["SmolLM3-3B", "huggingface"],
  ["all-MiniLM-L6-v2", "huggingface"],
  ["bge-m3", "baai"],
  ["jina-embeddings-v4", "jina"],
  ["voyage-3-large", "voyage"],
  ["nomic-embed-text-v1.5", "nomic"],
  ["mxbai-embed-large", "mixedbread"],
];

describe("providerForModel", () => {
  it.each(CASES)("maps %s to %s", (name, id) => {
    expect(providerForModel(name)?.id).toBe(id);
  });

  it("covers every provider in the table", () => {
    const tested = new Set(CASES.map(([, id]) => id));
    const missing = ALL_PROVIDERS.map((p) => p.id).filter((id) => !tested.has(id));
    expect(missing).toEqual([]);
  });

  it("matches on the upstream when the served name is opaque", () => {
    expect(providerForModel("house-chat", "http://gpu01:8000/v1 (Qwen3-8B)")?.id).toBe("qwen");
  });

  it("attributes a fine-tune to the base-model vendor when both are in the name", () => {
    // First pattern wins, and the base vendor sits higher in the table — a
    // Llama-derived Hermes reads as Meta rather than Nous.
    expect(providerForModel("Hermes-4-Llama-3.1-405B")?.id).toBe("meta");
  });

  it("anchors the short `muse` alias so it cannot swallow unrelated names", () => {
    // `muse` is an ordinary English word, so the table anchors it with \b on
    // both sides: a hyphen still terminates it, but a longer word does not.
    expect(providerForModel("muse-glimmer-30b")?.id).toBe("meta");
    expect(providerForModel("amuse-bouche-7b")).toBeNull();
    expect(providerForModel("musegen-1b")).toBeNull();
  });

  it("returns null for names with no known vendor", () => {
    expect(providerForModel("house-model-v2")).toBeNull();
    expect(providerForModel("")).toBeNull();
    expect(providerForModel(null, undefined)).toBeNull();
  });
});

describe("provider logos", () => {
  it.each(ALL_PROVIDERS)("$id has a logo asset on disk", ({ src }) => {
    expect(src.startsWith("/providers/")).toBe(true);
    expect(existsSync(path.join(process.cwd(), "public", src))).toBe(true);
  });
});
