import type {
  AuditEntry,
  CapacityDiscoveryView,
  FairshareLiveView,
  FamilyMembers,
  LifecycleStatus,
  ModelFamily,
  ModelFamilyWrite,
  ModelHealthSummary,
  ModelLifecycle,
  ModelReasoning,
  ModelRoute,
  ModelUsageTimePoint,
  ReasoningAction,
  ReasoningLevel,
  ReasoningMode,
  UsageModelAgg,
} from "@/lib/obleth";
import { healthState, isBenchmarkRoute, type HealthState } from "@/lib/overview-model";
import { money, perMillion } from "@/lib/format";
import { poolOccupancy } from "@/lib/occupancy";
import { parseTagLevel, TAG_LEVEL_LABELS } from "@/lib/utils";

/**
 * The Models pages' pure logic: list rows, filters and sorting, price and
 * status wording, the setting index behind "Find a setting", and the change
 * tracking behind the settings page's one save bar.
 */

// ---------------------------------------------------------------------------
// Types and where a model runs
// ---------------------------------------------------------------------------

export type TypeGroup = "chat" | "image" | "video" | "mesh" | "audio" | "embedding" | "search";

export const TYPE_GROUPS: { value: TypeGroup; label: string }[] = [
  { value: "chat", label: "Chat" },
  { value: "image", label: "Image" },
  { value: "video", label: "Video" },
  { value: "mesh", label: "3D" },
  { value: "audio", label: "Audio" },
  { value: "embedding", label: "Embed" },
  { value: "search", label: "Search" },
];

export function typeGroup(type: string): TypeGroup {
  if (type === "audio_transcription" || type === "audio_speech") return "audio";
  if (type === "image" || type === "video" || type === "mesh" || type === "embedding" || type === "search") return type;
  return "chat";
}

export const MODEL_TYPE_NAMES: Record<string, string> = {
  chat: "Chat",
  embedding: "Embeddings",
  audio_transcription: "Speech to text",
  audio_speech: "Text to speech",
  image: "Image",
  video: "Video",
  mesh: "3D mesh",
  search: "Web search",
};

export type RunsOn = "kubernetes" | "slurm" | "endpoint";

export const RUNS_LABELS: Record<RunsOn, string> = {
  kubernetes: "Kubernetes",
  slurm: "Slurm",
  endpoint: "External endpoint",
};

/** Where the replicas come from: a Slurm-managed spec, a Kubernetes Service it counts, or an endpoint it was pointed at. */
export function runsOn(model: Pick<ModelRoute, "capacity_mode" | "capacity_source">, managed: boolean): RunsOn {
  if (managed) return "slurm";
  if (model.capacity_mode === "discovered" && model.capacity_source === "kubernetes") return "kubernetes";
  return "endpoint";
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/** One word for where a model stands. `off` is a route switched off, whatever its checks say. */
export type ModelStatus = "serving" | "down" | "maintenance" | "unchecked" | "off";

export const STATUS_LABELS: Record<ModelStatus, string> = {
  serving: "Serving",
  down: "Failing checks",
  maintenance: "Maintenance",
  unchecked: "Not checked yet",
  off: "Off",
};

export function modelStatus(model: Pick<ModelRoute, "enabled">, row: ModelHealthSummary | undefined, now = Date.now()): ModelStatus {
  if (!model.enabled) return "off";
  const state = healthState(row, now);
  if (state === "maintenance") return "maintenance";
  if (state === "unhealthy") return "down";
  if (state === "healthy") return "serving";
  return "unchecked";
}

/** The header line: "46 of 48 serving · 1 in maintenance · 2 off". Switched-off models count separately. */
export function statusLine(rows: Pick<ModelRow, "status">[]): string {
  const count = (s: ModelStatus) => rows.filter((r) => r.status === s).length;
  const parts = [`${count("serving")} of ${rows.length - count("off")} serving`];
  if (count("down")) parts.push(`${count("down")} failing checks`);
  if (count("maintenance")) parts.push(`${count("maintenance")} in maintenance`);
  if (count("unchecked")) parts.push(`${count("unchecked")} not checked yet`);
  if (count("off")) parts.push(`${count("off")} off`);
  return parts.join(" · ");
}

/**
 * Whether a switched-off model has yet to pass a health check. The scheduler
 * records a switched-off model's checks as `disabled`, so only the probes
 * that actually ran (a create's first check, "Check and turn on") count.
 */
export function awaitingFirstPass<T extends { status: string }>(checks: T[]): { waiting: boolean; last: T | null } {
  const probed = checks.filter((c) => c.status !== "disabled");
  return { waiting: !probed.some((c) => c.status === "healthy"), last: probed[0] ?? null };
}

// ---------------------------------------------------------------------------
// Prices and labels
// ---------------------------------------------------------------------------

/** The model's price in the unit its type bills by, or null when none is set. */
export function priceLabel(model: Pick<ModelRoute, "model_type" | "input_cost_per_token" | "output_cost_per_token" | "cost_per_image" | "cost_per_video" | "cost_per_character" | "cost_per_audio_second">): string | null {
  const price = (v: number) => money(v, { precise: true });
  switch (model.model_type) {
    case "image":
      return model.cost_per_image > 0 ? `${price(model.cost_per_image)} / image` : null;
    case "video":
      return model.cost_per_video > 0 ? `${price(model.cost_per_video)} / video` : null;
    case "mesh":
      return model.cost_per_video > 0 ? `${price(model.cost_per_video)} / mesh` : null;
    case "audio_speech":
      return model.cost_per_character > 0 ? `${perMillion(model.cost_per_character)} / 1M chars` : null;
    case "audio_transcription":
      return model.cost_per_audio_second > 0 ? `${price(model.cost_per_audio_second * 60)} / min` : null;
    case "embedding":
      return model.input_cost_per_token > 0 ? `${perMillion(model.input_cost_per_token)} / 1M` : null;
    case "search":
      return null;
    default:
      if (model.input_cost_per_token <= 0 && model.output_cost_per_token <= 0) return null;
      return `${perMillion(model.input_cost_per_token)} · ${perMillion(model.output_cost_per_token)}`;
  }
}

export function contextLabel(tokens: number): string {
  if (!tokens) return "—";
  if (tokens >= 1_000_000) return `${+(tokens / 1_000_000).toFixed(1)}M`;
  if (tokens >= 1000) return `${Math.round(tokens / 1024) * 1024 === tokens ? tokens / 1024 : Math.round(tokens / 1000)}K`;
  return String(tokens);
}

/** Router tags as short labels: "coding · reasoning (Best)". */
export function tagLabels(tags: string[] | undefined): string[] {
  return (tags ?? []).map(parseTagLevel).map((t) => (t.declared ? `${t.base} (${TAG_LEVEL_LABELS[t.level]})` : t.base));
}

// ---------------------------------------------------------------------------
// List rows
// ---------------------------------------------------------------------------

export interface ModelRow {
  id: string;
  name: string;
  model: ModelRoute;
  group: TypeGroup;
  status: ModelStatus;
  health: HealthState;
  runs: RunsOn;
  inFlight: number;
  cap: number;
  queued: number;
  requests: number;
  p50TtftMs: number;
  price: string | null;
}

export function buildModelRows(
  models: ModelRoute[],
  health: ModelHealthSummary[],
  managed: Record<string, boolean>,
  usage: UsageModelAgg[] = [],
  fairshare?: FairshareLiveView,
  discovery?: CapacityDiscoveryView,
  now = Date.now(),
): ModelRow[] {
  const byId = new Map(health.map((h) => [h.model_id, h]));
  const byName = new Map(usage.map((u) => [u.model, u]));
  return models.map((model) => {
    const row = byId.get(model.id);
    const pool = poolOccupancy(model.model_name, model, fairshare, discovery);
    const u = byName.get(model.model_name);
    return {
      id: model.id,
      name: model.model_name,
      model,
      group: typeGroup(model.model_type),
      status: modelStatus(model, row, now),
      health: healthState(row, now),
      runs: runsOn(model, managed[model.id] ?? false),
      ...pool,
      requests: Number(u?.requests ?? 0),
      p50TtftMs: Number(u?.p50_ttft_ms ?? 0),
      price: priceLabel(model),
    };
  });
}

export type StatusFilter = "all" | "attention" | "off";
export type ModelSort = "busiest" | "requests" | "name" | "newest";

export const SORT_LABELS: Record<ModelSort, string> = {
  busiest: "Busiest now",
  requests: "Most requests",
  name: "Name",
  newest: "Newest",
};

export interface ModelFilters {
  query: string;
  group: TypeGroup | "all";
  status: StatusFilter;
  runs: RunsOn | "all";
  /** Benchmark and mock routes stay out of the way unless asked for. */
  benchmarks: boolean;
}

export const EMPTY_FILTERS: ModelFilters = { query: "", group: "all", status: "all", runs: "all", benchmarks: false };

/** Name, alias, variant, upstream, description or tag: every word of the query must match one of them. */
export function matchesQuery(model: ModelRoute, query: string): boolean {
  const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return true;
  const hay = [model.model_name, model.upstream_model, model.description, model.api_base, ...(model.aliases ?? []), ...(model.variants ?? []).map((v) => v.name), ...(model.tags ?? []).map((t) => parseTagLevel(t).base)]
    .join(" ")
    .toLowerCase();
  return words.every((w) => hay.includes(w));
}

export function filterRows(rows: ModelRow[], f: ModelFilters): ModelRow[] {
  return rows.filter((r) => {
    if (!f.benchmarks && isBenchmarkRoute(r.model)) return false;
    if (f.group !== "all" && r.group !== f.group) return false;
    if (f.runs !== "all" && r.runs !== f.runs) return false;
    if (f.status === "attention" && !(r.status === "down" || (r.cap > 0 && r.queued > 0))) return false;
    if (f.status === "off" && r.status !== "off") return false;
    return matchesQuery(r.model, f.query);
  });
}

const STATUS_ORDER: Record<ModelStatus, number> = { down: 0, serving: 1, unchecked: 2, maintenance: 3, off: 4 };

export function sortRows(rows: ModelRow[], sort: ModelSort): ModelRow[] {
  const load = (r: ModelRow) => (r.cap > 0 ? r.inFlight / r.cap : 0);
  const byName = (a: ModelRow, b: ModelRow) => a.name.localeCompare(b.name);
  const sorted = [...rows];
  if (sort === "name") return sorted.sort(byName);
  if (sort === "newest") return sorted.sort((a, b) => (b.model.created_at || "").localeCompare(a.model.created_at || "") || byName(a, b));
  if (sort === "requests") return sorted.sort((a, b) => b.requests - a.requests || byName(a, b));
  // Busiest: problems first, then the fullest pools, then traffic.
  return sorted.sort((a, b) =>
    STATUS_ORDER[a.status] - STATUS_ORDER[b.status] ||
    b.queued - a.queued ||
    load(b) - load(a) ||
    b.inFlight - a.inFlight ||
    b.requests - a.requests ||
    byName(a, b),
  );
}

export function groupCounts(rows: ModelRow[]): Record<TypeGroup | "all", number> {
  const counts = { all: rows.length, chat: 0, image: 0, video: 0, mesh: 0, audio: 0, embedding: 0, search: 0 };
  for (const r of rows) counts[r.group] += 1;
  return counts;
}

export { modelHref } from "@/lib/model-links";

// ---------------------------------------------------------------------------
// A model's own page: its traffic
// ---------------------------------------------------------------------------

/** Half-hour buckets over two days: today's line and yesterday's behind it. */
export const MODEL_SERIES_BUCKET_MS = 1_800_000;

export interface ModelTenantRow {
  name: string;
  requests: number;
  tokens: number;
}

/** What a model's own page shows about its traffic, one read for the Overview section. */
export interface ModelOverviewData {
  now: number;
  usage: UsageModelAgg | null;
  /** Requests in the 24 hours before the last 24, or null when the read failed. */
  previousRequests: number | null;
  series: ModelUsageTimePoint[];
  tenants: ModelTenantRow[];
  audit: AuditEntry[];
}

// ---------------------------------------------------------------------------
// Settings: sections, the setting index, and change tracking
// ---------------------------------------------------------------------------

export type SettingsSectionId =
  | "general"
  | "connection"
  | "routing"
  | "capabilities"
  | "pricing"
  | "capacity"
  | "health"
  | "reasoning"
  | "endpoints"
  | "deployment";

export const SECTIONS: { id: SettingsSectionId; label: string }[] = [
  { id: "general", label: "General" },
  { id: "connection", label: "Connection" },
  { id: "routing", label: "Routing" },
  { id: "capabilities", label: "Capabilities" },
  { id: "pricing", label: "Pricing" },
  { id: "capacity", label: "Capacity" },
  { id: "health", label: "Health" },
  { id: "reasoning", label: "Reasoning" },
  { id: "endpoints", label: "Endpoints" },
  { id: "deployment", label: "Deployment" },
];

export interface SettingEntry {
  /** The element id the search jumps to. */
  id: string;
  label: string;
  section: SettingsSectionId;
  /** Other words people look for it by. */
  keywords?: string;
  /** Only for these model types; absent = every type. */
  types?: string[];
}

const CHAT = ["chat"];
const TEXT = ["chat", "embedding"];

export const SETTING_INDEX: SettingEntry[] = [
  { id: "set-description", label: "Description", section: "general" },
  { id: "set-type", label: "Model type", section: "general", keywords: "modality endpoint chat embedding image video audio" },
  { id: "set-aliases", label: "Aliases", section: "general", keywords: "other names rename old name" },
  { id: "set-listed", label: "List in /v1/models", section: "general", keywords: "hide hidden discovery models list advertise family" },
  { id: "set-variants", label: "Variants", section: "general", keywords: "other names extra boons speculation spec", types: CHAT },
  { id: "set-quantization", label: "Quantization", section: "general", keywords: "format fp8 bf16 mxfp4 precision" },
  { id: "set-upstream", label: "Upstream model", section: "connection", keywords: "native name served model" },
  { id: "set-api-base", label: "API base URL", section: "connection", keywords: "url endpoint address host" },
  { id: "set-api-key", label: "Upstream API key", section: "connection", keywords: "secret token auth bearer" },
  { id: "set-headers", label: "Upstream headers", section: "connection", keywords: "header routing hint" },
  { id: "set-rejected-fields", label: "Rejected request fields", section: "connection", keywords: "block refuse unsupported parameter field option" },
  { id: "set-timeout", label: "Request timeout", section: "connection", keywords: "timeout seconds slow" },
  { id: "set-retries", label: "Max retries", section: "connection", keywords: "retry failover attempts backoff" },
  { id: "set-selection", label: "Endpoint selection", section: "connection", keywords: "failover load balance session sticky" },
  { id: "set-debug", label: "Debug upstream failures", section: "connection", keywords: "diagnostics 502 504 logs" },
  { id: "set-auto", label: "Eligible for auto", section: "routing", keywords: "auto router exclude", types: CHAT },
  { id: "set-tags", label: "Router tags", section: "routing", keywords: "coding reasoning math vision level strength", types: CHAT },
  { id: "set-bias", label: "Routing bias", section: "routing", keywords: "preference score nudge", types: CHAT },
  { id: "set-context", label: "Context window", section: "capabilities", keywords: "tokens length max context", types: TEXT },
  { id: "set-native", label: "Native capabilities", section: "capabilities", keywords: "function calling tools tool choice schema json system messages", types: CHAT },
  { id: "set-boons", label: "Boons", section: "capabilities", keywords: "vision structured output compression knowledge image generation speculation", types: CHAT },
  { id: "set-tools", label: "Tool servers", section: "capabilities", keywords: "mcp tools grants", types: CHAT },
  { id: "set-price", label: "Price", section: "pricing", keywords: "cost billing per token per image per video" },
  { id: "set-energy", label: "Energy slots per node", section: "pricing", keywords: "energy power co2 accounting" },
  { id: "set-capacity-mode", label: "Capacity mode", section: "capacity", keywords: "static tuned discovered pool size" },
  { id: "set-max-slots", label: "Max slots", section: "capacity", keywords: "concurrency in flight max_in_flight cap limit" },
  { id: "set-discovery", label: "Discovered capacity", section: "capacity", keywords: "kubernetes service replicas per replica headroom" },
  { id: "set-weight", label: "Admission weight", section: "capacity", keywords: "priority share weight" },
  { id: "set-affinity", label: "Session affinity", section: "capacity", keywords: "agent session sticky warm hold prompt cache fairshare" },
  { id: "set-cache", label: "Response cache", section: "capacity", keywords: "cache ttl hit" },
  { id: "set-autotune", label: "Auto-tune capacity", section: "capacity", keywords: "probe knee benchmark", types: TEXT },
  { id: "set-checks", label: "Health checks", section: "health", keywords: "interval probe scheduled alerts slack" },
  { id: "set-threshold", label: "Failure threshold", section: "health", keywords: "failures alert" },
  { id: "set-maintenance", label: "Maintenance window", section: "health", keywords: "maintenance pause note" },
  { id: "reasoning", label: "Reasoning effort", section: "reasoning", keywords: "thinking reasoning_effort enable_thinking levels none low medium high translate", types: CHAT },
  { id: "endpoints", label: "Endpoints", section: "endpoints", keywords: "clusters priority weight add endpoint" },
  { id: "deployment", label: "Deployment", section: "deployment", keywords: "slurm replicas provisioning launch" },
];

export const SECTION_LABEL: Record<SettingsSectionId, string> = Object.fromEntries(SECTIONS.map((s) => [s.id, s.label])) as Record<SettingsSectionId, string>;

/** Settings matching a search, best first: label prefix, then label, then keywords. */
export function searchSettings(query: string, modelType: string, limit = 6): SettingEntry[] {
  const q = query.trim().toLowerCase();
  if (!q) return [];
  const applies = SETTING_INDEX.filter((e) => !e.types || e.types.includes(modelType));
  const score = (e: SettingEntry) => {
    const label = e.label.toLowerCase();
    if (label.startsWith(q)) return 0;
    if (label.split(/\s+/).some((w) => w.startsWith(q))) return 1;
    if (label.includes(q)) return 2;
    if ((e.keywords ?? "").split(/\s+/).some((w) => w.startsWith(q))) return 3;
    if (SECTION_LABEL[e.section].toLowerCase().startsWith(q)) return 4;
    return 9;
  };
  return applies
    .map((e) => ({ e, s: score(e) }))
    .filter((x) => x.s < 9)
    .sort((a, b) => a.s - b.s)
    .slice(0, limit)
    .map((x) => x.e);
}

/** Which save call owns a form field; everything unlisted is part of the model itself. */
export function fieldSection(name: string): "model" | "cache" | "reliability" | "health" | "capacity" | "knowledge" | "listing" | null {
  if (name === "id" || name === "sections" || name.startsWith("has_") || name === "knowledge_loaded") return null;
  if (name === "cache_enabled" || name === "cache_ttl_secs") return "cache";
  if (["request_timeout_secs", "max_retries", "retry_backoff_ms", "endpoint_selection_mode", "debug_diagnostics"].includes(name)) return "reliability";
  if (["check_interval_secs", "failure_threshold", "maintenance_until", "maintenance_note", "checks_enabled", "alerts_enabled"].includes(name)) return "health";
  if (name === "max_in_flight" || name.startsWith("capacity_") || name === "per_replica_max_in_flight") return "capacity";
  if (name === "knowledge_collection") return "knowledge";
  if (name === "listed") return "listing";
  return "model";
}

const FIELD_LABELS: [RegExp, string][] = [
  [/^description$/, "Description"],
  [/^model_type$/, "Model type"],
  [/^aliases$/, "Aliases"],
  [/^listed$/, "Listed in /v1/models"],
  [/^variants$/, "Variants"],
  [/^quantization$/, "Quantization"],
  [/^upstream_model$/, "Upstream model"],
  [/^api_base$/, "API base URL"],
  [/^api_key$/, "Upstream API key"],
  [/^upstream_headers$/, "Upstream headers"],
  [/^rejected_request_fields$/, "Rejected request fields"],
  [/^session_affinity_/, "Session affinity"],
  [/^request_timeout_secs$/, "Request timeout"],
  [/^(max_retries|retry_backoff_ms)$/, "Retries"],
  [/^endpoint_selection_mode$/, "Endpoint selection"],
  [/^debug_diagnostics$/, "Debug upstream failures"],
  [/^auto_eligible$/, "Eligible for auto"],
  [/^tag_/, "Router tags"],
  [/^route_bias$/, "Routing bias"],
  [/^context_window$/, "Context window"],
  [/^supports_/, "Native capabilities"],
  [/^boon_/, "Boons"],
  [/^(draft_model|verify_api_base|verify_upstream_model)$/, "Speculation"],
  [/^tool_server_/, "Tool servers"],
  [/^knowledge_collection$/, "Knowledge collections"],
  [/cost_per/, "Price"],
  [/^energy_slots_per_node$/, "Energy slots"],
  [/^capacity_mode$/, "Capacity mode"],
  [/^max_in_flight$/, "Max slots"],
  [/^(capacity_source|capacity_service|capacity_namespace|per_replica_max_in_flight|capacity_headroom)$/, "Discovered capacity"],
  [/^admission_weight$/, "Admission weight"],
  [/^(cache_enabled|cache_ttl_secs)$/, "Response cache"],
  [/^(checks_enabled|alerts_enabled|check_interval_secs)$/, "Health checks"],
  [/^failure_threshold$/, "Failure threshold"],
  [/^maintenance_/, "Maintenance window"],
];

export function fieldLabel(name: string): string {
  return FIELD_LABELS.find(([re]) => re.test(name))?.[1] ?? name.replace(/_/g, " ");
}

/** A form's fields as name → every value it submits, joined, so two snapshots compare as strings. */
export type FormSnapshot = Map<string, string>;

export function snapshotForm(data: FormData): FormSnapshot {
  const out: FormSnapshot = new Map();
  for (const [name, value] of data.entries()) {
    if (typeof value !== "string") continue;
    out.set(name, out.has(name) ? `${out.get(name)}\u0000${value}` : value);
  }
  return out;
}

/** Field names whose submitted value differs. A checkbox that submits nothing when clear counts as changed when it flips. */
export function changedFields(base: FormSnapshot, next: FormSnapshot): string[] {
  const names = new Set([...base.keys(), ...next.keys()]);
  return [...names].filter((n) => fieldSection(n) !== null && (base.get(n) ?? "") !== (next.get(n) ?? "")).sort();
}

export function changedSections(fields: string[]): string[] {
  return [...new Set(fields.map(fieldSection).filter((s): s is NonNullable<typeof s> => s !== null))];
}

/** The save bar's summary: distinct labels, in page order. */
export function changeSummary(fields: string[]): string[] {
  const order = FIELD_LABELS.map(([, label]) => label);
  return [...new Set(fields.map(fieldLabel))].sort((a, b) => order.indexOf(a) - order.indexOf(b));
}

/** Whether any of a setting's fields (exact names, or prefixes ending in `_`) changed. */
export function touches(changed: string[], fields: string[]): boolean {
  return changed.some((c) => fields.some((f) => (f.endsWith("_") ? c.startsWith(f) : c === f)));
}

// ---------------------------------------------------------------------------
// Variants: more names for a model, each with extra boons on
// ---------------------------------------------------------------------------

/** The most variants the gateway keeps for one model. */
export const MAX_VARIANTS = 8;

/** One variant row as the settings page edits it. */
export interface VariantDraft {
  name: string;
  description: string;
  boons: string[];
}

export function variantDrafts(model: Pick<ModelRoute, "variants">): VariantDraft[] {
  return (model.variants ?? []).map((v) => ({ name: v.name, description: v.description ?? "", boons: [...(v.boons ?? [])] }));
}

/**
 * The variant rows as the settings form submits them: one JSON field, names
 * and descriptions trimmed and boons in the editor's order, so an edit put
 * back reads as no change. Boons the editor doesn't know keep their place at
 * the end.
 */
export function variantsValue(rows: VariantDraft[], boonOrder: readonly string[]): string {
  const rank = (b: string) => (boonOrder.includes(b) ? boonOrder.indexOf(b) : boonOrder.length);
  return JSON.stringify(
    rows.map((r) => ({
      name: r.name.trim(),
      description: r.description.trim(),
      boons: [...new Set(r.boons)].sort((a, b) => rank(a) - rank(b)),
    })),
  );
}

/**
 * Why row `index`'s name can't be saved, in a sentence, or null. A blank name
 * is left to the field's own required check. The gateway checks again,
 * against every model's names, aliases and variants.
 */
export function variantNameProblem(
  rows: VariantDraft[],
  index: number,
  own: { name: string; aliases: string[]; otherModels: string[] },
): string | null {
  const name = rows[index]?.name.trim() ?? "";
  if (!name) return null;
  if (name === "auto") return "auto is the router's own name.";
  if (name === own.name) return "That is this model's own name.";
  if (own.aliases.includes(name)) return "That is already one of this model's aliases.";
  if (own.otherModels.includes(name)) return "Another model is already called that.";
  if (rows.some((r, i) => i < index && r.name.trim() === name)) return "Another variant already has that name.";
  return null;
}

/**
 * Whether a model can take an image in a chat request: it reads images itself,
 * or it opted into the vision boon and the boon is on, so the gateway describes
 * each image for it. The auto router uses the same rule. `undefined` while the
 * model isn't known yet, so callers can hold back or not.
 */
export function acceptsImages(
  model: Pick<ModelRoute, "supports_vision" | "boons"> | undefined,
  visionBoonActive: boolean,
): boolean | undefined {
  if (!model) return undefined;
  return model.supports_vision || (visionBoonActive && (model.boons ?? []).includes("vision"));
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/** "Oct 19, 2026", read in UTC like the gateway's dates. */
export function lifecycleDate(iso: string): string {
  return new Date(iso).toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric", timeZone: "UTC" });
}

/** The one-word badge for each status in the models list. */
export const LIFECYCLE_BADGES: Record<LifecycleStatus, string> = {
  staged: "Staged",
  active: "Active",
  deprecated: "Deprecated",
  retired: "Retired",
};

/** The status in force now, defaulting to active on a gateway without lifecycles. */
export function lifecycleStatus(model: Pick<ModelRoute, "lifecycle" | "effective_status">): LifecycleStatus {
  return model.effective_status ?? model.lifecycle?.status ?? "active";
}

/**
 * The header pill for a model that is not plainly active, or null when it is:
 * "Staged · not listed yet", "Deprecated · retires Oct 19, 2026", "Retired",
 * "Retired · answered by gemma4-31b-it".
 */
export function lifecycleLabel(model: Pick<ModelRoute, "lifecycle" | "effective_status">): string | null {
  const status = lifecycleStatus(model);
  const l = model.lifecycle;
  if (status === "staged") return "Staged · not listed yet";
  if (status === "deprecated") return l?.retire_at ? `Deprecated · retires ${lifecycleDate(l.retire_at)}` : "Deprecated";
  if (status === "retired") return l?.redirect && l.replacement ? `Retired · answered by ${l.replacement}` : "Retired";
  return null;
}

/**
 * What a caller is told when a retired model refuses them. Mirrors
 * `retired_message` in the gateway's `lifecycle.rs`, so the page can show it
 * before anyone sees it for real.
 */
export function retiredMessage(modelName: string, l: Pick<ModelLifecycle, "status" | "replacement" | "retire_at" | "changed_at" | "note">): string {
  const on = l.retire_at ?? (l.status === "retired" ? l.changed_at : null);
  let msg = on ? `The model \`${modelName}\` was retired on ${on.slice(0, 10)}.` : `The model \`${modelName}\` has been retired.`;
  if (l.replacement) msg += ` Use \`${l.replacement}\` instead.`;
  if (l.note) msg += ` ${l.note}`;
  return msg;
}

/** A stored RFC 3339 date as a date input's value (YYYY-MM-DD, UTC). */
export function dateInputValue(iso?: string | null): string {
  return iso ? iso.slice(0, 10) : "";
}

/** A date input's value as the start of that day in UTC, or null when blank. */
export function retireAtFromInput(value: string): string | null {
  return /^\d{4}-\d{2}-\d{2}$/.test(value) ? `${value}T00:00:00Z` : null;
}

// ---------------------------------------------------------------------------
// Reasoning effort
// ---------------------------------------------------------------------------

export const REASONING_LEVELS: readonly ReasoningLevel[] = ["none", "low", "medium", "high"];

export const REASONING_LEVEL_LABELS: Record<ReasoningLevel, string> = { none: "None", low: "Low", medium: "Medium", high: "High" };

/** Request fields a level may not set. Mirrors `RESERVED_REASONING_FIELDS` in the gateway's `reasoning.rs`. */
export const RESERVED_REASONING_FIELDS = ["model", "messages", "prompt", "input", "stream", "stream_options", "tools", "tool_choice", "n"];

/** The most one level may send, as compact JSON. */
export const MAX_REASONING_SEND_BYTES = 4096;

/** A model's map with what older gateways leave out filled in: pass-through, no default, no levels. */
export function modelReasoning(model: Pick<ModelRoute, "reasoning">): { mode: ReasoningMode; default_level: ReasoningLevel | null; levels: Partial<Record<ReasoningLevel, ReasoningAction>> } {
  const r = model.reasoning;
  return { mode: r?.mode ?? "passthrough", default_level: r?.default_level ?? null, levels: { ...(r?.levels ?? {}) } };
}

/** The section list's word for a model that does something with effort, or undefined when it passes it through. */
export function reasoningNavCount(model: Pick<ModelRoute, "reasoning">): string | undefined {
  const mode = modelReasoning(model).mode;
  return mode === "mapped" ? "translated" : mode === "unsupported" ? "removed" : undefined;
}

/** The ways one level can be translated, as the Reasoning section offers them. */
export type LevelPreset = "default" | "effort" | "thinking_off" | "thinking_on" | "use" | "custom";

export const LEVEL_PRESETS: { value: LevelPreset; label: string }[] = [
  { value: "default", label: "Server default (send nothing)" },
  { value: "effort", label: "Send reasoning_effort =" },
  { value: "thinking_off", label: "Turn thinking off (enable_thinking: false)" },
  { value: "thinking_on", label: "Turn thinking on (enable_thinking: true)" },
  { value: "use", label: "Same as another level" },
  { value: "custom", label: "Custom fields (JSON)" },
];

/** One level's row as the section edits it. Every input keeps its text, so switching presets back and forth loses nothing. */
export interface LevelDraft {
  preset: LevelPreset;
  effort: string;
  use: ReasoningLevel;
  json: string;
}

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);

/** The level next to `level`, the first choice for "Same as another level". */
function neighbour(level: ReasoningLevel): ReasoningLevel {
  const i = REASONING_LEVELS.indexOf(level);
  return REASONING_LEVELS[i === REASONING_LEVELS.length - 1 ? i - 1 : i + 1];
}

/**
 * A stored action as a row: the preset it matches exactly, or Custom with its
 * JSON. A level with nothing stored starts as `reasoning_effort` = its own name.
 */
export function levelDraft(level: ReasoningLevel, action?: ReasoningAction): LevelDraft {
  const draft: LevelDraft = { preset: "effort", effort: level, use: neighbour(level), json: "{}" };
  if (!action) return draft;
  if ("use" in action) return { ...draft, preset: "use", use: action.use };
  const send = isObject(action.send) ? action.send : {};
  const keys = Object.keys(send);
  const json = JSON.stringify(send, null, 2);
  if (keys.length === 0) return { ...draft, preset: "default", json };
  if (keys.length === 1 && typeof send.reasoning_effort === "string") return { ...draft, preset: "effort", effort: send.reasoning_effort, json };
  const kwargs = send.chat_template_kwargs;
  if (keys.length === 1 && isObject(kwargs) && Object.keys(kwargs).length === 1 && typeof kwargs.enable_thinking === "boolean") {
    return { ...draft, preset: kwargs.enable_thinking ? "thinking_on" : "thinking_off", json };
  }
  return { ...draft, preset: "custom", json };
}

export type LevelBuild = { ok: true; action: ReasoningAction } | { ok: false; error: string };

/** A row as the action the gateway stores, or why it can't be one. */
export function levelAction(draft: LevelDraft): LevelBuild {
  switch (draft.preset) {
    case "default":
      return { ok: true, action: { send: {} } };
    case "effort": {
      const value = draft.effort.trim();
      if (!value) return { ok: false, error: "Type the value to send as reasoning_effort, such as low or high." };
      return { ok: true, action: { send: { reasoning_effort: value } } };
    }
    case "thinking_off":
      return { ok: true, action: { send: { chat_template_kwargs: { enable_thinking: false } } } };
    case "thinking_on":
      return { ok: true, action: { send: { chat_template_kwargs: { enable_thinking: true } } } };
    case "use":
      return { ok: true, action: { use: draft.use } };
    case "custom": {
      let parsed: unknown;
      try {
        parsed = JSON.parse(draft.json);
      } catch {
        return { ok: false, error: "The custom fields are not valid JSON. Write an object in braces, such as {\"reasoning_effort\": \"high\"}." };
      }
      if (!isObject(parsed)) return { ok: false, error: "The custom fields must be a JSON object in braces, such as {\"reasoning_effort\": \"high\"}." };
      return { ok: true, action: { send: parsed } };
    }
  }
}

/**
 * Why the gateway would refuse a map, in a sentence, or null. Mirrors
 * `ModelReasoning::validate` and the chat-only rule in the admin API, so the
 * section can say so before saving; the gateway checks again.
 */
export function reasoningProblem(r: ModelReasoning, modelType?: string): string | null {
  const levels = r.levels ?? {};
  const label = (l: ReasoningLevel) => REASONING_LEVEL_LABELS[l];
  if (r.mode !== "passthrough" && modelType !== undefined && modelType !== "chat") return "Only chat models take reasoning effort. Leave this model on pass through.";
  for (const level of REASONING_LEVELS) {
    const action = levels[level];
    if (!action) continue;
    if ("use" in action) {
      if (action.use === level) return `${label(level)} can't be the same as itself. Pick another level.`;
      if (r.mode !== "mapped") continue;
      const target = levels[action.use];
      if (!target) return `${label(level)} is the same as ${label(action.use)}, but ${label(action.use)} has nothing set.`;
      if ("use" in target) return `${label(level)} is the same as ${label(action.use)}, which is itself the same as another level. Point it at a level that sends something.`;
      continue;
    }
    const field = RESERVED_REASONING_FIELDS.find((f) => f in action.send);
    if (field) return `${label(level)} may not set ${field}. A level can only send fields about effort.`;
    const bytes = new TextEncoder().encode(JSON.stringify(action.send)).length;
    if (bytes > MAX_REASONING_SEND_BYTES) return `${label(level)} sends ${bytes} bytes of fields; one level can send at most ${MAX_REASONING_SEND_BYTES}.`;
  }
  if (r.mode === "mapped") {
    const missing = REASONING_LEVELS.filter((l) => !levels[l]);
    if (missing.length) return `Set all four levels to translate them. Not set: ${missing.join(", ")}.`;
  }
  return null;
}

/** The Reasoning section's state. */
export interface ReasoningDraft {
  mode: ReasoningMode;
  /** "" leaves a request without effort to the model's server. */
  defaultLevel: ReasoningLevel | "";
  levels: Record<ReasoningLevel, LevelDraft>;
}

export function reasoningDraft(model: Pick<ModelRoute, "reasoning">): ReasoningDraft {
  const r = modelReasoning(model);
  return {
    mode: r.mode,
    defaultLevel: r.default_level ?? "",
    levels: Object.fromEntries(REASONING_LEVELS.map((l) => [l, levelDraft(l, r.levels[l])])) as Record<ReasoningLevel, LevelDraft>,
  };
}

/** Object keys sorted at every depth, so two maps compare as strings. */
function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (isObject(value)) return `{${Object.keys(value).sort().map((k) => `${JSON.stringify(k)}:${canonical(value[k])}`).join(",")}}`;
  return JSON.stringify(value);
}

/** The stored map in the shape the section writes, for comparing. */
function storedForm(model: Pick<ModelRoute, "reasoning">): ModelReasoning {
  const r = modelReasoning(model);
  return { mode: r.mode, ...(r.default_level ? { default_level: r.default_level } : {}), ...(Object.keys(r.levels).length ? { levels: r.levels } : {}) };
}

export type ReasoningBuild = { ok: true; reasoning: ModelReasoning } | { ok: false; error: string };

/**
 * The section's state as the map to save. The level rows are written when
 * translating, or when someone changed them; otherwise the stored levels go
 * back as they were, so switching to pass through keeps them for later and
 * opening the page never invents levels.
 */
export function reasoningFromDraft(draft: ReasoningDraft, model: Pick<ModelRoute, "reasoning" | "model_type">): ReasoningBuild {
  const stored = modelReasoning(model);
  const initial = reasoningDraft(model);
  const touched = REASONING_LEVELS.some((l) => canonical(levelAction(draft.levels[l])) !== canonical(levelAction(initial.levels[l])));
  let levels: Partial<Record<ReasoningLevel, ReasoningAction>> = stored.levels;
  if (draft.mode === "mapped" || touched) {
    levels = {};
    for (const level of REASONING_LEVELS) {
      const built = levelAction(draft.levels[level]);
      if (!built.ok) return { ok: false, error: `${REASONING_LEVEL_LABELS[level]}: ${built.error}` };
      levels[level] = built.action;
    }
  }
  const reasoning: ModelReasoning = {
    mode: draft.mode,
    ...(draft.defaultLevel ? { default_level: draft.defaultLevel } : {}),
    ...(Object.keys(levels).length ? { levels } : {}),
  };
  const problem = reasoningProblem(reasoning, model.model_type);
  return problem ? { ok: false, error: problem } : { ok: true, reasoning };
}

/** Whether the section holds something other than what is stored. A row that can't be saved counts as a change. */
export function reasoningDirty(draft: ReasoningDraft, model: Pick<ModelRoute, "reasoning" | "model_type">): boolean {
  const built = reasoningFromDraft(draft, model);
  if (!built.ok) return canonical(draft) !== canonical(reasoningDraft(model));
  return canonical(built.reasoning) !== canonical(storedForm(model));
}

/** Fields as "a.b: value" pairs, nested objects flattened. */
function flatFields(obj: Record<string, unknown>, prefix = ""): string[] {
  return Object.entries(obj).flatMap(([k, v]) =>
    isObject(v) && Object.keys(v).length > 0 ? flatFields(v, `${prefix}${k}.`) : [`${prefix}${k}: ${typeof v === "string" ? v : JSON.stringify(v)}`],
  );
}

/** What a level's fields do to a request: "sends reasoning_effort: max", or "sends nothing, so the server's own default applies". */
export function sendWords(send: Record<string, unknown>): string {
  const fields = flatFields(send);
  return fields.length ? `sends ${fields.join(", ")}` : "sends nothing, so the server's own default applies";
}

/**
 * The live preview: one line per level for a translated model ("high → sends
 * reasoning_effort: max", "none → served as low, which sends …"), then what
 * happens when a caller names no level. One sentence for the other modes.
 */
export function reasoningPreview(r: ModelReasoning): string[] {
  if (r.mode === "passthrough") return ["Every request reaches the model as the caller sent it, effort included."];
  if (r.mode === "unsupported") return ["obleth removes reasoning_effort and reasoning from every request, so the model's server never sees them."];
  const levels = r.levels ?? {};
  const lines = REASONING_LEVELS.map((level) => {
    const action = levels[level];
    if (!action) return `${level} → not set yet`;
    if (!("use" in action)) return `${level} → ${sendWords(action.send)}`;
    const target = levels[action.use];
    const then = target && !("use" in target) ? `, which ${sendWords(target.send)}` : "";
    return `${level} → served as ${action.use}${then}${level === "none" ? " (this model has no off switch)" : ""}`;
  });
  lines.push(r.default_level ? `no effort set → served as ${r.default_level}` : "no effort set → sent as it came, so the server's own default applies");
  return lines;
}

// ---------------------------------------------------------------------------
// Model families: one name, a model per reasoning level
// ---------------------------------------------------------------------------

/** Longest family name and description the gateway keeps. */
export const MAX_FAMILY_NAME_CHARS = 128;
export const MAX_FAMILY_DESCRIPTION_CHARS = 500;

/** A family as its editor holds it. */
export interface FamilyDraft {
  name: string;
  description: string;
  default_level: ReasoningLevel;
  members: FamilyMembers;
}

export function familyDraft(family?: ModelFamily | null): FamilyDraft {
  if (!family) return { name: "", description: "", default_level: "none", members: { none: "", low: "", medium: "", high: "" } };
  return { name: family.name, description: family.description ?? "", default_level: family.default_level, members: { ...family.members } };
}

/** The editor's state as the gateway takes it, names and description trimmed. */
export function familyWrite(draft: FamilyDraft): ModelFamilyWrite {
  return {
    name: draft.name.trim(),
    description: draft.description.trim(),
    default_level: draft.default_level,
    members: Object.fromEntries(REASONING_LEVELS.map((l) => [l, draft.members[l].trim()])) as unknown as FamilyMembers,
  };
}

/** Every name a model answers to: its own, its aliases and its variants. A family may use none of them. */
export function takenModelNames(models: Pick<ModelRoute, "model_name" | "aliases" | "variants">[]): string[] {
  return models.flatMap((m) => [m.model_name, ...(m.aliases ?? []), ...(m.variants ?? []).map((v) => v.name)]);
}

/** Why a family name can't be saved, in a sentence, or null. Mirrors the gateway's checks; it checks again. */
export function familyNameProblem(name: string, taken: { modelNames: string[]; familyNames: string[] }): string | null {
  const n = name.trim();
  if (!n) return "Give the family a name. Callers send it as the model.";
  if (/\s/.test(n)) return "A family name can't contain spaces.";
  if (n.length > MAX_FAMILY_NAME_CHARS) return `Keep the name to ${MAX_FAMILY_NAME_CHARS} characters.`;
  if (n === "auto") return "auto is the router's own name.";
  if (taken.modelNames.includes(n)) return "A model already answers to that name.";
  if (taken.familyNames.includes(n)) return "Another family already has that name.";
  return null;
}

/** Why a family can't be saved, or null: its name, its description, then the first level with no model. */
export function familyProblem(draft: FamilyDraft, taken: { modelNames: string[]; familyNames: string[] }): string | null {
  const name = familyNameProblem(draft.name, taken);
  if (name) return name;
  if (draft.description.trim().length > MAX_FAMILY_DESCRIPTION_CHARS) return `Keep the description to ${MAX_FAMILY_DESCRIPTION_CHARS} characters.`;
  const missing = REASONING_LEVELS.find((l) => !draft.members[l].trim());
  return missing ? `Pick the model that answers at ${missing}.` : null;
}

/** The levels each member answers, in level order: [{ levels: ["none"], model: "a" }, { levels: ["low", "medium", "high"], model: "b" }]. */
export function familyGroups(members: FamilyMembers): { levels: ReasoningLevel[]; model: string }[] {
  const groups: { levels: ReasoningLevel[]; model: string }[] = [];
  for (const level of REASONING_LEVELS) {
    const model = members[level];
    const group = groups.find((g) => g.model === model);
    if (group) group.levels.push(level);
    else groups.push({ levels: [level], model });
  }
  return groups;
}

/**
 * A family in plain words: "A request for acme-7b with no effort set goes to
 * acme-7b-instruct." and "none → acme-7b-instruct · low, medium, high →
 * acme-7b-think". Null until every level has a model.
 */
export function familyPreview(f: Pick<FamilyDraft, "name" | "default_level" | "members">): { lead: string; levels: string } | null {
  if (REASONING_LEVELS.some((l) => !f.members[l].trim())) return null;
  const name = f.name.trim() || "this family";
  return {
    lead: `A request for ${name} with no effort set goes to ${f.members[f.default_level]}.`,
    levels: familyGroups(f.members).map((g) => `${g.levels.join(", ")} → ${g.model}`).join(" · "),
  };
}

/**
 * The models a family may use: chat models that are still served, sorted,
 * deprecated ones marked. A member already chosen stays on the list whatever
 * its state, so the editor can show it.
 */
export function familyMemberOptions(
  models: Pick<ModelRoute, "model_name" | "model_type" | "lifecycle" | "effective_status">[],
  chosen: string[] = [],
): { value: string; label: string }[] {
  const options = models
    .filter((m) => m.model_type === "chat" && (lifecycleStatus(m) !== "retired" || chosen.includes(m.model_name)))
    .map((m) => {
      const status = lifecycleStatus(m);
      return { value: m.model_name, label: status === "active" ? m.model_name : `${m.model_name} (${status})` };
    });
  for (const name of chosen) {
    if (name && !options.some((o) => o.value === name)) options.push({ value: name, label: `${name} (not a chat model)` });
  }
  return options.sort((a, b) => a.value.localeCompare(b.value));
}
