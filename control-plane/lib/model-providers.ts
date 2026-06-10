// Maps model names to provider logos under public/providers/. Most are LobeHub
// dark-theme monochrome marks; a few fall back to vLLM recipe avatars where
// no dark icon exists. Matching is keyword-based on model/upstream names.

export interface ModelProvider {
  id: string;
  label: string;
  src: string;
}

const PROVIDERS: Array<{ pattern: RegExp; provider: ModelProvider }> = [
  { pattern: /qwen|qwq|qvq/, provider: { id: "qwen", label: "Qwen", src: "/providers/qwen.png" } },
  { pattern: /gemma|gemini|paligemma/, provider: { id: "google", label: "Google", src: "/providers/google.png" } },
  { pattern: /llama/, provider: { id: "meta", label: "Meta", src: "/providers/meta.png" } },
  { pattern: /minimax/, provider: { id: "minimax", label: "MiniMax", src: "/providers/minimax.png" } },
  { pattern: /mistral|mixtral|devstral|magistral|ministral|codestral|pixtral/, provider: { id: "mistral", label: "Mistral AI", src: "/providers/mistral.png" } },
  { pattern: /gpt|whisper|dall-e|o[134](?:-mini)?\b/, provider: { id: "openai", label: "OpenAI", src: "/providers/openai.png" } },
  { pattern: /\bglm|chatglm|cogview|cogvideo/, provider: { id: "zai", label: "Z.ai", src: "/providers/zai.png" } },
  { pattern: /granite/, provider: { id: "ibm", label: "IBM", src: "/providers/ibm.png" } },
  { pattern: /deepseek/, provider: { id: "deepseek", label: "DeepSeek", src: "/providers/deepseek.png" } },
  { pattern: /kimi|moonshot/, provider: { id: "moonshot", label: "Moonshot AI", src: "/providers/moonshot.png" } },
  { pattern: /\bphi-?\d/, provider: { id: "microsoft", label: "Microsoft", src: "/providers/microsoft.png" } },
  { pattern: /nemotron/, provider: { id: "nvidia", label: "NVIDIA", src: "/providers/nvidia.png" } },
  { pattern: /internlm|intern-s/, provider: { id: "internlm", label: "InternLM", src: "/providers/internlm.png" } },
  { pattern: /hunyuan/, provider: { id: "tencent", label: "Tencent", src: "/providers/tencent.png" } },
  { pattern: /ernie/, provider: { id: "baidu", label: "Baidu", src: "/providers/baidu.png" } },
  { pattern: /doubao|bytedance|\bseed-/, provider: { id: "bytedance", label: "ByteDance", src: "/providers/bytedance.png" } },
  { pattern: /\bstep-?\d/, provider: { id: "stepfun", label: "StepFun", src: "/providers/stepfun.png" } },
  { pattern: /mimo/, provider: { id: "xiaomi", label: "Xiaomi", src: "/providers/xiaomi.png" } },
  { pattern: /\bring-|\bling-|bailing/, provider: { id: "inclusionai", label: "inclusionAI", src: "/providers/inclusionai.png" } },
  { pattern: /minicpm/, provider: { id: "openbmb", label: "OpenBMB", src: "/providers/openbmb.png" } },
  { pattern: /mellum/, provider: { id: "jetbrains", label: "JetBrains", src: "/providers/jetbrains.png" } },
];

export function providerForModel(...names: Array<string | null | undefined>): ModelProvider | null {
  const haystack = names.filter(Boolean).join(" ").toLowerCase();
  if (!haystack) return null;
  for (const { pattern, provider } of PROVIDERS) {
    if (pattern.test(haystack)) return provider;
  }
  return null;
}
