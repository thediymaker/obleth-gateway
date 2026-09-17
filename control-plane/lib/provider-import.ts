import { normalizeModelApiNameFinal } from "./model-name";

export interface UpstreamModel {
  id: string;
  owned_by?: string;
}

export function normalizeBase(base: string): string {
  return base.trim().replace(/\/+$/, "");
}

// Accepts the OpenAI `{ object:"list", data:[...] }` shape or a bare array.
// Keeps only entries with a non-empty string id, dedupes, sorts ascending.
export function parseUpstreamModelList(json: unknown): UpstreamModel[] {
  const arr: unknown[] = Array.isArray(json)
    ? json
    : json && typeof json === "object" && Array.isArray((json as { data?: unknown }).data)
      ? (json as { data: unknown[] }).data
      : [];

  const seen = new Set<string>();
  const out: UpstreamModel[] = [];
  for (const entry of arr) {
    if (!entry || typeof entry !== "object") continue;
    const id = (entry as { id?: unknown }).id;
    if (typeof id !== "string" || id.trim() === "") continue;
    if (seen.has(id)) continue;
    seen.add(id);
    const ownedBy = (entry as { owned_by?: unknown }).owned_by;
    out.push({ id, owned_by: typeof ownedBy === "string" ? ownedBy : undefined });
  }
  out.sort((a, b) => a.id.localeCompare(b.id));
  return out;
}

export type DiscoveredStatus = "new" | "existing";

export interface DiscoveredRow {
  id: string;
  modelName: string;
  ownedBy?: string;
  status: DiscoveredStatus;
  /**
   * Serving format read off the upstream id, from the gateway's fixed
   * `QUANTIZATIONS` vocabulary. "unknown" when the id says nothing about it.
   */
  quantization: string;
  /**
   * The suffix that was lifted out of `modelName` into `quantization`, if
   * any — offered as an alias so a client already calling the provider by its
   * full id keeps working through the gateway.
   */
  suggestedAlias?: string;
}

// Quantization tokens as they actually appear in provider model ids, mapped to
// the gateway's vocabulary value. Longest first, so `nvfp4`/`mxfp4` are not
// read as a bare `fp4`, and `w4a16-awq` lands on `awq`.
const QUANT_TOKENS: readonly { token: string; value: string }[] = [
  { token: "nvfp4", value: "nvfp4" },
  { token: "mxfp4", value: "mxfp4" },
  { token: "gptq", value: "gptq" },
  { token: "gguf", value: "gguf" },
  { token: "bf16", value: "bf16" },
  { token: "fp16", value: "fp16" },
  { token: "int8", value: "int8" },
  { token: "int4", value: "int4" },
  { token: "fp8", value: "fp8" },
  { token: "awq", value: "awq" },
];

/**
 * Split a normalized model name into the clean name and the serving format its
 * suffix declared.
 *
 * Providers ship the format in the id (`Qwen3-8B-FP8`, `glm-5-3-mxfp4`), which
 * is exactly what makes a registered name brittle: re-quantizing the deployment
 * forces a rename, and every pinned client breaks. So the token is lifted out
 * of the name into the `quantization` field, and the full original is offered
 * back as an alias.
 *
 * Only a trailing token is stripped, and only when something is left over — a
 * model genuinely named `fp8` keeps its name. Callers may always override: the
 * wizard shows the suggestion in an editable field.
 */
export function splitQuantizationSuffix(modelName: string): {
  name: string;
  quantization: string;
} {
  for (const { token, value } of QUANT_TOKENS) {
    // Dot is a legal separator in an obleth model name, so accept either.
    const m = modelName.match(new RegExp(`^(.+?)[.-]${token}$`));
    if (m && m[1]) return { name: m[1], quantization: value };
  }
  return { name: modelName, quantization: "unknown" };
}

export interface ExistingRouteRef {
  model_name: string;
  upstream_model: string;
  api_base: string;
}

// A discovered model is "existing" when an existing route matches by normalized
// model_name, OR by the same (normalized api_base + upstream_model) pair. A
// space joins the pair key fields.
export function classifyDiscovered(
  models: UpstreamModel[],
  existing: ExistingRouteRef[],
  base: string,
): DiscoveredRow[] {
  const normBase = normalizeBase(base);
  const names = new Set(existing.map((r) => r.model_name));
  const pairs = new Set(
    existing.map((r) => `${normalizeBase(r.api_base)} ${r.upstream_model}`),
  );
  return models.map((m) => {
    const full = normalizeModelApiNameFinal(m.id);
    const { name: modelName, quantization } = splitQuantizationSuffix(full);
    // "Already registered" is judged on both spellings: a route created before
    // the suffix was split out still carries the full name, and re-importing it
    // must not read as a new model.
    const exists =
      names.has(modelName) || names.has(full) || pairs.has(`${normBase} ${m.id}`);
    return {
      id: m.id,
      modelName,
      ownedBy: m.owned_by,
      status: exists ? "existing" : "new",
      quantization,
      suggestedAlias: modelName === full ? undefined : full,
    };
  });
}

export interface BatchDefaults {
  model_type: string;
  context_window?: number;
  input_cost_per_token?: number;
  output_cost_per_token?: number;
  cost_per_image?: number;
  cost_per_audio_second?: number;
  cost_per_character?: number;
  admission_weight?: number;
  tags?: string[];
  enabled: boolean;
  description?: string;
}

export interface RowState {
  id: string;
  modelName: string;
  included: boolean;
  overrides: Partial<BatchDefaults>;
  /** Vocabulary value for this row, seeded from the upstream id. */
  quantization?: string;
  /** Aliases to register alongside the clean name. */
  aliases?: string[];
}

function stripUndefined<T extends object>(obj: T): Partial<T> {
  return Object.fromEntries(
    Object.entries(obj).filter(([, v]) => v !== undefined),
  ) as Partial<T>;
}

// Merges batch defaults with per-row overrides for each included row, producing
// a bare `{ models: [...] }` document — the shape `applyModelManifestAction`
// accepts alongside a full manifest. Serialized to JSON, it flows through the
// same dry-run-then-apply path as an uploaded file.
// Note: the import updates routes whose model_name already exists, so the
// additive (create-only) guarantee is enforced by the caller — the wizard
// excludes already-existing models from selection. This builder does not itself
// prevent updates.
export function buildImportPayload(
  rows: RowState[],
  base: string,
  apiKey: string | undefined,
  defaults: BatchDefaults,
): { models: Record<string, unknown>[] } {
  const normBase = normalizeBase(base);
  const models = rows
    .filter((r) => r.included)
    .map((r) => {
      const merged = { ...defaults, ...stripUndefined(r.overrides) };
      const entry: Record<string, unknown> = {
        model_name: normalizeModelApiNameFinal(r.modelName),
        upstream_model: r.id,
        api_base: normBase,
        model_type: merged.model_type,
        enabled: merged.enabled,
      };
      if (r.quantization && r.quantization !== "unknown") entry.quantization = r.quantization;
      // Only ever non-empty when the name was cleaned up, so the provider's own
      // id keeps resolving.
      if (r.aliases && r.aliases.length > 0) entry.aliases = r.aliases;
      if (apiKey) entry.api_key = apiKey;
      if (merged.description) entry.description = merged.description;
      if (merged.context_window != null) entry.context_window = merged.context_window;
      if (merged.input_cost_per_token != null) entry.input_cost_per_token = merged.input_cost_per_token;
      if (merged.output_cost_per_token != null) entry.output_cost_per_token = merged.output_cost_per_token;
      if (merged.cost_per_image != null) entry.cost_per_image = merged.cost_per_image;
      if (merged.cost_per_audio_second != null) entry.cost_per_audio_second = merged.cost_per_audio_second;
      if (merged.cost_per_character != null) entry.cost_per_character = merged.cost_per_character;
      if (merged.admission_weight != null) entry.admission_weight = merged.admission_weight;
      if (merged.tags && merged.tags.length > 0) entry.tags = merged.tags;
      return entry;
    });
  return { models };
}
