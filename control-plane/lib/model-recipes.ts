// Slurm launch-recipe engine (Open OnDemand-style).
//
// obleth has no per-backend "provider" type: a managed model is just a raw
// `launch_command` (+ optional `preamble`/env) that the provisioner runs, either
// inside an apptainer image or bare-metal when the image is blank. A *recipe*
// is the UI layer that turns a handful of typed knobs into that command, so an
// admin can stand up a backend (e.g. llama.cpp's `--n-cpu-moe`) without writing
// shell by hand.
//
// Recipes are pure, serializable DATA — no functions — so they can be authored
// in editable YAML files (see `recipe-files.ts`), loaded on the server, and
// passed into the client wizard as props. The command/preamble are produced by
// the pure composer functions at the bottom (`buildRecipeCommand`,
// `buildRecipePreamble`), which both the server and the browser can call.
//
// `manual: true` is the Custom escape hatch — no templating.

export type RecipeParamKind = "number" | "text" | "select" | "boolean";

/**
 * How a single parameter contributes to the launch command (or environment).
 * Exactly one of `flag`/`flags`, `switch`, `env`, or `raw` should be set. An
 * empty value (after trimming) is always omitted from the command line.
 */
export type RecipeArg = {
  /** Emit `<flag> <value>`. */
  flag?: string;
  /** Emit `<f> <value>` for every flag (e.g. paired `--cache-type-k/-v`). */
  flags?: string[];
  /** Values that suppress emission even when non-empty (e.g. `"0"`, `"auto"`). */
  omitWhen?: string[];
  /** Boolean switch: emit this flag when the value is "true". */
  switch?: string;
  /** Optional value appended after a `switch` flag (e.g. `--flash-attn on`). */
  switchValue?: string;
  /** Export an environment variable (into the preamble) instead of a CLI flag. */
  env?: string;
  /** Value for the exported env var (defaults to "1" for booleans, else the param value). */
  envValue?: string;
  /** Append the value verbatim (e.g. a free-form "extra args" field). */
  raw?: boolean;
};

export type RecipeParam = {
  id: string;
  label: string;
  kind: RecipeParamKind;
  /** Default value, always stored as a string (booleans use "true"/"false"). */
  default: string;
  options?: { value: string; label: string }[];
  hint?: string;
  placeholder?: string;
  /** Render under "Advanced" instead of inline. */
  advanced?: boolean;
  /** How this knob maps onto the command line / environment. */
  arg?: RecipeArg;
};

/** Structured description of the launch command for the common case. */
export type RecipeCommand = {
  /** The binary or launcher to invoke (e.g. `vllm`, `llama-server`, or an absolute path). */
  executable: string;
  /** Sub-commands emitted right after the executable (e.g. vLLM's `serve`). */
  prefixArgs?: string[];
  /** Flag that carries the model value; empty means the model is positional. */
  modelFlag?: string;
  /** Fixed args emitted after the model (e.g. `--host 0.0.0.0`). */
  fixedArgs?: string[];
  /** Flag that carries the serving port. */
  portFlag: string;
  /** When set and the upstream name is present, emit `<aliasFlag> <upstream>` always. */
  aliasFlag?: string;
  /** When set, emit `<servedNameFlag> <upstream>` only when the upstream differs from the model. */
  servedNameFlag?: string;
};

export type RecipeBuildContext = {
  /** Backend model handle: GGUF path (llama.cpp), HF id (vLLM) or tag (Ollama). */
  model: string;
  port: string;
  /** Name obleth sends upstream in the `model` field; used for alias matching. */
  upstreamModel: string;
  values: Record<string, string>;
};

export type SlurmRecipe = {
  id: string;
  label: string;
  badge: string;
  hint: string;
  healthPath: string;
  modelLabel?: string;
  modelPlaceholder?: string;
  modelHint?: string;
  imagePlaceholder?: string;
  imageHint?: string;
  /** When true, the apptainer image may be blank (run bare-metal). */
  imageOptional?: boolean;
  /** Custom backend: operator writes the launch command directly. */
  manual?: boolean;
  params?: RecipeParam[];
  /** Structured launch command (ignored when `commandTemplate` is set). */
  command?: RecipeCommand;
  /**
   * Escape hatch for backends that don't fit the structured model (e.g. Ollama's
   * serve-then-pull shell). Tokens: `{{model}}`, `{{port}}`, `{{upstream}}`.
   */
  commandTemplate?: string;
  /** Static environment exports always added to the preamble. */
  env?: Record<string, string>;
  /**
   * Raw shell lines prepended to the job script before the launch command
   * (e.g. `module load cuda`). Emitted ahead of any `env` exports.
   */
  preamble?: string;
};

/** Initial value map for a recipe's parameters. */
export function recipeDefaults(recipe: SlurmRecipe): Record<string, string> {
  const out: Record<string, string> = {};
  for (const p of recipe.params ?? []) out[p.id] = p.default;
  return out;
}

const v = (values: Record<string, string>, id: string) => (values[id] ?? "").trim();

/** Whether a parameter's value should contribute (non-empty and not suppressed). */
function isActive(param: RecipeParam, value: string): boolean {
  if (param.kind === "boolean") return value === "true";
  if (value === "") return false;
  return !(param.arg?.omitWhen ?? []).includes(value);
}

function substitute(template: string, ctx: RecipeBuildContext): string {
  return template
    .replaceAll("{{model}}", ctx.model || "<model>")
    .replaceAll("{{port}}", ctx.port || "8000")
    .replaceAll("{{upstream}}", ctx.upstreamModel || "");
}

/** Render the launch command for a recipe + parameter values. */
export function buildRecipeCommand(recipe: SlurmRecipe, ctx: RecipeBuildContext): string {
  if (recipe.manual) return "";
  if (recipe.commandTemplate) return substitute(recipe.commandTemplate, ctx);
  const cmd = recipe.command;
  if (!cmd) return "";

  const parts: string[] = [cmd.executable, ...(cmd.prefixArgs ?? [])];
  const model = ctx.model || "<model>";
  if (cmd.modelFlag) parts.push(cmd.modelFlag, model);
  else parts.push(model);
  if (cmd.fixedArgs) parts.push(...cmd.fixedArgs);
  parts.push(cmd.portFlag, ctx.port || "8000");
  if (cmd.aliasFlag && ctx.upstreamModel) {
    parts.push(cmd.aliasFlag, ctx.upstreamModel);
  }
  if (cmd.servedNameFlag && ctx.upstreamModel && ctx.upstreamModel !== ctx.model) {
    parts.push(cmd.servedNameFlag, ctx.upstreamModel);
  }

  for (const param of recipe.params ?? []) {
    const arg = param.arg;
    if (!arg) continue;
    const value = v(ctx.values, param.id);
    if (arg.env) continue; // handled by buildRecipePreamble
    if (arg.raw) {
      if (value !== "") parts.push(value);
      continue;
    }
    if (arg.switch) {
      if (value === "true") {
        parts.push(arg.switch);
        if (arg.switchValue) parts.push(arg.switchValue);
      }
      continue;
    }
    const flags = arg.flags ?? (arg.flag ? [arg.flag] : []);
    if (flags.length && isActive(param, value)) {
      for (const f of flags) parts.push(f, value);
    }
  }
  return parts.join(" ");
}

/** Render the env-export preamble (params with `env` args + static `env`). */
export function buildRecipePreamble(
  recipe: SlurmRecipe,
  values: Record<string, string>,
): string {
  const lines: string[] = [];
  const preamble = (recipe.preamble ?? "").trim();
  if (preamble) lines.push(preamble);
  for (const [name, value] of Object.entries(recipe.env ?? {})) {
    lines.push(`export ${name}=${value}`);
  }
  for (const param of recipe.params ?? []) {
    const arg = param.arg;
    if (!arg?.env) continue;
    const value = v(values, param.id);
    if (!isActive(param, value)) continue;
    const exported = arg.envValue ?? (param.kind === "boolean" ? "1" : value);
    lines.push(`export ${arg.env}=${exported}`);
  }
  return lines.join("\n");
}

export const SLURM_RECIPES: readonly SlurmRecipe[] = [
  {
    id: "vllm",
    label: "vLLM",
    badge: "GPU optimised",
    hint: "High-throughput PagedAttention serving",
    healthPath: "/health",
    modelPlaceholder: "Qwen/Qwen3-8B",
    modelHint: "HuggingFace model ID passed to vllm serve.",
    imagePlaceholder: "/shared/images/vllm.sif",
    command: {
      executable: "vllm",
      prefixArgs: ["serve"],
      portFlag: "--port",
      servedNameFlag: "--served-model-name",
    },
    params: [
      {
        id: "tensor_parallel",
        label: "Tensor parallel size",
        kind: "number",
        default: "1",
        hint: "GPUs to shard the model across (1 = single GPU).",
        advanced: true,
        arg: { flag: "--tensor-parallel-size", omitWhen: ["1"] },
      },
      {
        id: "max_model_len",
        label: "Max model length",
        kind: "number",
        default: "",
        placeholder: "model default",
        advanced: true,
        arg: { flag: "--max-model-len" },
      },
      {
        id: "gpu_memory_utilization",
        label: "GPU memory utilization",
        kind: "text",
        default: "",
        placeholder: "0.90",
        advanced: true,
        arg: { flag: "--gpu-memory-utilization" },
      },
      {
        id: "dtype",
        label: "Dtype",
        kind: "select",
        default: "auto",
        options: [
          { value: "auto", label: "auto" },
          { value: "bfloat16", label: "bfloat16" },
          { value: "float16", label: "float16" },
        ],
        advanced: true,
        arg: { flag: "--dtype", omitWhen: ["auto"] },
      },
      {
        id: "extra_args",
        label: "Extra args",
        kind: "text",
        default: "",
        placeholder: "--enable-prefix-caching",
        advanced: true,
        arg: { raw: true },
      },
    ],
  },
  {
    id: "ollama",
    label: "Ollama",
    badge: "Multi-model",
    hint: "Easy pull-and-serve, GGUF-friendly",
    healthPath: "/api/tags",
    modelPlaceholder: "qwen2.5:0.5b",
    modelHint: "Ollama model tag (e.g. from ollama pull).",
    imagePlaceholder: "/shared/images/ollama.sif",
    imageHint: "Build with: apptainer pull docker://ollama/ollama",
    commandTemplate:
      'sh -c "OLLAMA_HOST=0.0.0.0:{{port}} ollama serve & sleep 5 && OLLAMA_HOST=0.0.0.0:{{port}} ollama pull {{model}} && wait"',
  },
  {
    id: "llamacpp",
    label: "llama.cpp",
    badge: "GGUF / MoE",
    hint: "Single-GGUF serving, great for quantised & MoE models",
    healthPath: "/health",
    modelLabel: "GGUF model path",
    modelPlaceholder: "/shared/models/model.gguf",
    modelHint: "Path to the .gguf file passed to llama-server -m.",
    imagePlaceholder: "/shared/images/llamacpp.sif (optional)",
    imageHint: "Leave blank to run a native, module-loaded llama-server (no container).",
    imageOptional: true,
    command: {
      executable: "llama-server",
      modelFlag: "-m",
      fixedArgs: ["--host", "0.0.0.0"],
      portFlag: "--port",
      aliasFlag: "--alias",
    },
    params: [
      {
        id: "ngl",
        label: "GPU layers (-ngl)",
        kind: "number",
        default: "99",
        hint: "Layers offloaded to the GPU (99 = all).",
        arg: { flag: "-ngl" },
      },
      {
        id: "ctx_size",
        label: "Context size",
        kind: "number",
        default: "32768",
        hint: "KV cache is pre-allocated, so larger contexts reserve more RAM up front.",
        arg: { flag: "--ctx-size" },
      },
      {
        id: "n_cpu_moe",
        label: "CPU MoE layers (--n-cpu-moe)",
        kind: "number",
        default: "0",
        hint: "Keep expert tensors of N layers on the CPU (0 = off). Raise to fit large MoE models in VRAM.",
        arg: { flag: "--n-cpu-moe", omitWhen: ["0"] },
      },
      {
        id: "cache_type",
        label: "KV cache type",
        kind: "select",
        default: "f16",
        options: [
          { value: "f16", label: "f16 (full)" },
          { value: "q8_0", label: "q8_0 (smaller)" },
          { value: "q4_0", label: "q4_0 (smallest)" },
        ],
        hint: "Quantise the KV cache to fit a larger context.",
        arg: { flags: ["--cache-type-k", "--cache-type-v"], omitWhen: ["f16"] },
      },
      {
        id: "unified_memory",
        label: "CUDA unified memory (GH200)",
        kind: "boolean",
        default: "false",
        hint: "Let GPU allocations spill into the Grace CPU's coherent LPDDR.",
        arg: { env: "GGML_CUDA_ENABLE_UNIFIED_MEMORY", envValue: "1" },
      },
      {
        id: "parallel",
        label: "Parallel slots",
        kind: "number",
        default: "1",
        hint: "KV cache slots; 1 = the whole context is one sequence.",
        advanced: true,
        arg: { flag: "--parallel", omitWhen: ["1"] },
      },
      {
        id: "threads",
        label: "Threads",
        kind: "text",
        default: "",
        placeholder: "$(nproc)",
        advanced: true,
        arg: { flag: "--threads" },
      },
      {
        id: "batch_size",
        label: "Batch size",
        kind: "number",
        default: "",
        placeholder: "2048",
        advanced: true,
        arg: { flag: "--batch-size" },
      },
      {
        id: "ubatch_size",
        label: "U-batch size",
        kind: "number",
        default: "",
        placeholder: "512",
        advanced: true,
        arg: { flag: "--ubatch-size" },
      },
      {
        id: "flash_attn",
        label: "Flash attention",
        kind: "boolean",
        default: "true",
        advanced: true,
        arg: { switch: "--flash-attn", switchValue: "on" },
      },
      {
        id: "jinja",
        label: "Jinja chat template",
        kind: "boolean",
        default: "true",
        advanced: true,
        arg: { switch: "--jinja" },
      },
      {
        id: "no_mmap",
        label: "Disable mmap (--no-mmap)",
        kind: "boolean",
        default: "false",
        hint: "Sequential read instead of mmap; faster on a network filesystem.",
        advanced: true,
        arg: { switch: "--no-mmap" },
      },
      {
        id: "extra_args",
        label: "Extra args",
        kind: "text",
        default: "",
        placeholder: "--temp 1.0 --top-p 0.95",
        advanced: true,
        arg: { raw: true },
      },
    ],
  },
  {
    id: "custom",
    label: "Custom",
    badge: "Manual",
    hint: "Write your own launch command",
    healthPath: "/health",
    manual: true,
  },
] as const;
