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
}

export interface ExistingRouteRef {
  model_name: string;
  upstream_model: string;
  api_base: string;
}

// A discovered model is "existing" when an existing route matches by normalized
// model_name, OR by the same (normalized api_base + upstream_model) pair. NUL
// joins the pair key so ids containing the separator can't collide.
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
    const modelName = normalizeModelApiNameFinal(m.id);
    const exists = names.has(modelName) || pairs.has(`${normBase} ${m.id}`);
    return { id: m.id, modelName, ownedBy: m.owned_by, status: exists ? "existing" : "new" };
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
}

function stripUndefined<T extends object>(obj: T): Partial<T> {
  return Object.fromEntries(
    Object.entries(obj).filter(([, v]) => v !== undefined),
  ) as Partial<T>;
}

// Merges batch defaults with per-row overrides for each included row, producing
// entries shaped exactly like the obleth models template (field names that
// `toImportInput` in app/actions.ts already coerces). Serializes to JSON and
// flows through the existing planModelImportAction / importModelsAction path.
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
        model_name: r.modelName,
        upstream_model: r.id,
        api_base: normBase,
        model_type: merged.model_type,
        enabled: merged.enabled,
      };
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
