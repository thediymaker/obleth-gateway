// What a Hugging Face repo holds, and which engines can serve it. The lookup
// runs on the server (no token: gated repos show as gated); the reading of the
// API's answer is pure so it can be tested without the network.

export type EngineId = "vllm" | "sglang" | "llamacpp" | "ollama";

export interface EngineFit {
  engine: EngineId;
  ok: boolean;
  /** Why it can or can't, in one line. */
  why: string;
  /** What the engine recipe's `model` input should be. */
  model?: string;
}

export interface HfModel {
  id: string;
  architecture: string | null;
  modelType: string | null;
  format: "safetensors" | "gguf" | "pytorch" | "unknown";
  /** e.g. "modelopt (NVFP4)", "fp8", "awq"; null for full precision or unknown. */
  quantization: string | null;
  weightsGb: number | null;
  files: number;
  license: string | null;
  gated: boolean;
  chatTemplate: boolean;
  /** GGUF quantizations in the repo, smallest first. */
  ggufQuants: { name: string; gb: number }[];
  engines: EngineFit[];
  /** The engine to preselect. */
  recommended: EngineId | null;
}

interface HfSibling {
  rfilename: string;
  size?: number;
  lfs?: { size?: number };
}

interface HfApiModel {
  id?: string;
  modelId?: string;
  siblings?: HfSibling[];
  config?: { architectures?: string[]; model_type?: string; quantization_config?: { quant_method?: string; quant_algo?: string }; tokenizer_config?: { chat_template?: unknown } };
  cardData?: { license?: string };
  gated?: boolean | string;
  tags?: string[];
}

const GB = 1e9;

function size(s: HfSibling): number {
  return s.size ?? s.lfs?.size ?? 0;
}

/** "UD-IQ2_M", "Q4_K_M", "BF16" from a GGUF path, or null. */
export function ggufQuant(path: string): string | null {
  const m = /(?:^|[-_./])((?:UD-)?(?:I?Q\d(?:_[A-Z0-9]+)*|BF16|F16|F32|MXFP4))(?:[-_./]|$)/i.exec(path);
  return m ? m[1].toUpperCase().replace(/^UD-/, "UD-") : null;
}

/** Read the model API's answer into what the launch flow needs. */
export function readHfModel(repo: string, api: HfApiModel): HfModel {
  const files = api.siblings ?? [];
  const safetensors = files.filter((f) => f.rfilename.endsWith(".safetensors"));
  const gguf = files.filter((f) => f.rfilename.endsWith(".gguf"));
  const pytorch = files.filter((f) => /\.(bin|pt|pth)$/.test(f.rfilename) && !f.rfilename.includes("training_args"));
  const format: HfModel["format"] = safetensors.length ? "safetensors" : gguf.length ? "gguf" : pytorch.length ? "pytorch" : "unknown";

  const quants = new Map<string, number>();
  for (const f of gguf) {
    const q = ggufQuant(f.rfilename);
    if (q) quants.set(q, (quants.get(q) ?? 0) + size(f));
  }
  const ggufQuants = [...quants.entries()].map(([name, bytes]) => ({ name, gb: Math.round((bytes / GB) * 10) / 10 })).sort((a, b) => a.gb - b.gb);

  const weightBytes = (format === "safetensors" ? safetensors : format === "pytorch" ? pytorch : []).reduce((n, f) => n + size(f), 0);
  const qc = api.config?.quantization_config;
  const quantization = qc?.quant_method ? `${qc.quant_method}${qc.quant_algo ? ` (${qc.quant_algo})` : ""}` : null;
  const architecture = api.config?.architectures?.[0] ?? null;
  const chatTemplate = !!api.config?.tokenizer_config?.chat_template || files.some((f) => f.rfilename === "chat_template.jinja" || f.rfilename === "chat_template.json");

  // A GGUF near the middle of the range is a sensible first pick.
  const pickQuant = ggufQuants.find((q) => q.name === "Q4_K_M") ?? ggufQuants[Math.floor((ggufQuants.length - 1) / 2)];
  const engines: EngineFit[] = [];
  const hfFormat = format === "safetensors" || format === "pytorch";
  const arch = architecture ? ` ${architecture}` : "";
  engines.push(hfFormat ? { engine: "vllm", ok: true, why: `Loads the ${format} weights; the image's vLLM must support${arch || " the architecture"}.`, model: repo } : { engine: "vllm", ok: false, why: format === "gguf" ? "This repo has GGUF files only; vLLM wants the original weights." : "No weights vLLM can load." });
  engines.push(hfFormat ? { engine: "sglang", ok: true, why: `Loads the ${format} weights; the image's SGLang must support${arch || " the architecture"}.`, model: repo } : { engine: "sglang", ok: false, why: format === "gguf" ? "This repo has GGUF files only." : "No weights SGLang can load." });
  engines.push(pickQuant ? { engine: "llamacpp", ok: true, why: `${ggufQuants.length} GGUF quantization${ggufQuants.length === 1 ? "" : "s"}, ${ggufQuants[0].gb} to ${ggufQuants[ggufQuants.length - 1].gb} GB.`, model: `${repo}:${pickQuant.name}` } : { engine: "llamacpp", ok: false, why: "Needs a GGUF file; this repo has none." });
  engines.push(pickQuant ? { engine: "ollama", ok: true, why: "Ollama can pull a GGUF straight from Hugging Face.", model: `hf.co/${repo}:${pickQuant.name}` } : { engine: "ollama", ok: false, why: "Ollama runs GGUF files; this repo has none." });

  return {
    id: api.id ?? api.modelId ?? repo,
    architecture,
    modelType: api.config?.model_type ?? null,
    format,
    quantization,
    weightsGb: weightBytes ? Math.round(weightBytes / GB) : pickQuant ? pickQuant.gb : null,
    files: files.length,
    license: api.cardData?.license ?? null,
    gated: !!api.gated,
    chatTemplate,
    ggufQuants,
    engines,
    recommended: hfFormat ? "vllm" : pickQuant ? "llamacpp" : null,
  };
}

/** "org/name" from a pasted id or huggingface.co URL; null when it isn't one. */
export function parseRepoId(input: string): string | null {
  const s = input.trim().replace(/^https?:\/\/(www\.)?huggingface\.co\//, "").replace(/[?#].*$/, "").replace(/\/(tree|blob|resolve)\/.*$/, "").replace(/\/+$/, "");
  return /^[A-Za-z0-9][\w.-]*\/[\w.-]+$/.test(s) ? s : null;
}

/** Fetch and read a repo. Throws with a sentence a person can act on. */
export async function lookupHfModel(input: string, fetchImpl: typeof fetch = fetch): Promise<HfModel> {
  const repo = parseRepoId(input);
  if (!repo) throw new Error("That doesn't look like a Hugging Face model: use org/name, e.g. Qwen/Qwen3-32B.");
  let res: Response;
  try {
    res = await fetchImpl(`https://huggingface.co/api/models/${repo}?blobs=true`, { headers: { accept: "application/json" }, signal: AbortSignal.timeout(10_000), cache: "no-store" });
  } catch {
    throw new Error("Couldn't reach huggingface.co from the dashboard. Pick an engine and type the model instead.");
  }
  if (res.status === 404 || res.status === 401) throw new Error(`No public model called ${repo} on Hugging Face.`);
  if (!res.ok) throw new Error(`Hugging Face answered ${res.status}. Try again, or pick an engine and type the model.`);
  return readHfModel(repo, (await res.json()) as HfApiModel);
}
