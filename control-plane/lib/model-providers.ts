// Maps model names to provider logos under public/providers/. Most are LobeHub
// dark-theme monochrome marks; a few fall back to vLLM recipe avatars where
// no dark icon exists, and a handful (Thinking Machines, Nomic, Mixedbread) are
// vendor marks re-tinted to white-on-transparent to match the rest.
// Matching is keyword-based on model/upstream names, first pattern wins — so
// keep fine-tune families (Hermes, Tulu) below the base-model vendors they
// build on, and keep anything short or ambiguous anchored with \b.

export interface ModelProvider {
  id: string;
  label: string;
  src: string;
}

const PROVIDERS: Array<{ pattern: RegExp; provider: ModelProvider }> = [
  { pattern: /qwen|qwq|qvq|\bgte-/, provider: { id: "qwen", label: "Qwen", src: "/providers/qwen.png" } },
  { pattern: /gemma|gemini|paligemma/, provider: { id: "google", label: "Google", src: "/providers/google.png" } },
  { pattern: /llama/, provider: { id: "meta", label: "Meta", src: "/providers/meta.png" } },
  { pattern: /minimax/, provider: { id: "minimax", label: "MiniMax", src: "/providers/minimax.png" } },
  { pattern: /mistral|mixtral|devstral|magistral|ministral|codestral|pixtral/, provider: { id: "mistral", label: "Mistral AI", src: "/providers/mistral.png" } },
  { pattern: /gpt|whisper|dall-e|o[134](?:-mini)?\b|text-embedding-(?:ada|3)/, provider: { id: "openai", label: "OpenAI", src: "/providers/openai.png" } },
  { pattern: /\bglm|chatglm|cogview|cogvideo/, provider: { id: "zai", label: "Z.ai", src: "/providers/zai.png" } },
  { pattern: /granite/, provider: { id: "ibm", label: "IBM", src: "/providers/ibm.png" } },
  { pattern: /deepseek/, provider: { id: "deepseek", label: "DeepSeek", src: "/providers/deepseek.png" } },
  { pattern: /kimi|moonshot/, provider: { id: "moonshot", label: "Moonshot AI", src: "/providers/moonshot.png" } },
  { pattern: /\bphi-?\d|\be5-(?:small|base|large)/, provider: { id: "microsoft", label: "Microsoft", src: "/providers/microsoft.png" } },
  { pattern: /nemotron|\bnv-embed/, provider: { id: "nvidia", label: "NVIDIA", src: "/providers/nvidia.png" } },
  { pattern: /internlm|intern-s/, provider: { id: "internlm", label: "InternLM", src: "/providers/internlm.png" } },
  { pattern: /hunyuan/, provider: { id: "tencent", label: "Tencent", src: "/providers/tencent.png" } },
  { pattern: /ernie/, provider: { id: "baidu", label: "Baidu", src: "/providers/baidu.png" } },
  { pattern: /doubao|bytedance|\bseed-/, provider: { id: "bytedance", label: "ByteDance", src: "/providers/bytedance.png" } },
  { pattern: /\bstep-?\d/, provider: { id: "stepfun", label: "StepFun", src: "/providers/stepfun.png" } },
  { pattern: /mimo/, provider: { id: "xiaomi", label: "Xiaomi", src: "/providers/xiaomi.png" } },
  { pattern: /\bring-|\bling-|bailing/, provider: { id: "inclusionai", label: "inclusionAI", src: "/providers/inclusionai.png" } },
  { pattern: /minicpm/, provider: { id: "openbmb", label: "OpenBMB", src: "/providers/openbmb.png" } },
  { pattern: /mellum/, provider: { id: "jetbrains", label: "JetBrains", src: "/providers/jetbrains.png" } },
  { pattern: /cohere|command-?[ar]\b|\bnorth\b|\baya-|\bembed-(?:english|multilingual|v\d)/, provider: { id: "cohere", label: "Cohere", src: "/providers/cohere.png" } },
  { pattern: /inkling/, provider: { id: "thinkingmachines", label: "Thinking Machines Lab", src: "/providers/thinkingmachines.png" } },
  { pattern: /laguna/, provider: { id: "poolside", label: "Poolside", src: "/providers/poolside.png" } },
  { pattern: /ornith/, provider: { id: "ornith", label: "Ornith AI", src: "/providers/ornith.png" } },
  { pattern: /\bflux/, provider: { id: "blackforestlabs", label: "Black Forest Labs", src: "/providers/blackforestlabs.png" } },
  { pattern: /\bgrok/, provider: { id: "xai", label: "xAI", src: "/providers/xai.png" } },
  { pattern: /claude/, provider: { id: "anthropic", label: "Anthropic", src: "/providers/anthropic.png" } },
  { pattern: /jamba/, provider: { id: "ai21", label: "AI21 Labs", src: "/providers/ai21.png" } },
  { pattern: /falcon/, provider: { id: "tii", label: "TII", src: "/providers/tii.png" } },
  { pattern: /\bolmo|molmo|\btulu/, provider: { id: "ai2", label: "Ai2", src: "/providers/ai2.png" } },
  { pattern: /\blfm\d?-/, provider: { id: "liquid", label: "Liquid AI", src: "/providers/liquid.png" } },
  { pattern: /hermes/, provider: { id: "nous", label: "Nous Research", src: "/providers/nous.png" } },
  { pattern: /stable-?(?:diffusion|lm|code)|\bsdxl\b/, provider: { id: "stability", label: "Stability AI", src: "/providers/stability.png" } },
  { pattern: /\byi-(?:\d|large|lightning|coder|vl)/, provider: { id: "01ai", label: "01.AI", src: "/providers/01ai.png" } },
  { pattern: /baichuan/, provider: { id: "baichuan", label: "Baichuan", src: "/providers/baichuan.png" } },
  { pattern: /\bsolar-/, provider: { id: "upstage", label: "Upstage", src: "/providers/upstage.png" } },
  { pattern: /exaone/, provider: { id: "lg", label: "LG AI Research", src: "/providers/lg.png" } },
  { pattern: /\barctic/, provider: { id: "snowflake", label: "Snowflake", src: "/providers/snowflake.png" } },
  { pattern: /\bnova-(?:micro|lite|pro|premier|sonic|canvas|reel)|\btitan-(?:embed|text|image)/, provider: { id: "amazon", label: "Amazon", src: "/providers/amazon.png" } },
  { pattern: /\bsonar\b/, provider: { id: "perplexity", label: "Perplexity", src: "/providers/perplexity.png" } },
  { pattern: /skywork/, provider: { id: "skywork", label: "Skywork", src: "/providers/skywork.png" } },
  { pattern: /pangu/, provider: { id: "huawei", label: "Huawei", src: "/providers/huawei.png" } },
  { pattern: /smollm|smolvlm|starcoder|zephyr|all-minilm|all-mpnet|sentence-transformers/, provider: { id: "huggingface", label: "Hugging Face", src: "/providers/huggingface.png" } },
  { pattern: /\bbge-/, provider: { id: "baai", label: "BAAI", src: "/providers/baai.png" } },
  { pattern: /\bjina-/, provider: { id: "jina", label: "Jina AI", src: "/providers/jina.png" } },
  { pattern: /\bvoyage-/, provider: { id: "voyage", label: "Voyage AI", src: "/providers/voyage.png" } },
  { pattern: /nomic/, provider: { id: "nomic", label: "Nomic AI", src: "/providers/nomic.png" } },
  { pattern: /mxbai/, provider: { id: "mixedbread", label: "Mixedbread", src: "/providers/mixedbread.png" } },
];

/** Every provider referenced by the table, deduped by id. */
export const ALL_PROVIDERS: ModelProvider[] = [
  ...new Map(PROVIDERS.map(({ provider }) => [provider.id, provider])).values(),
];

export function providerForModel(...names: Array<string | null | undefined>): ModelProvider | null {
  const haystack = names.filter(Boolean).join(" ").toLowerCase();
  if (!haystack) return null;
  for (const { pattern, provider } of PROVIDERS) {
    if (pattern.test(haystack)) return provider;
  }
  return null;
}
