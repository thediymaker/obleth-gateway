import type {
  AuditEntry,
  CapacityDiscoveryView,
  FairshareLiveView,
  ModelHealthSummary,
  ModelRoute,
  ModelUsageTimePoint,
  UsageModelAgg,
} from "@/lib/obleth";
import { healthState, isBenchmarkRoute, poolOccupancy, type HealthState } from "@/lib/overview-model";
import { parseTagLevel, TAG_LEVEL_LABELS } from "@/lib/utils";

/**
 * The Models pages' pure logic: list rows, filters and sorting, price and
 * status wording, the setting index behind "Find a setting", and the change
 * tracking behind the settings page's one save bar.
 */

// ---------------------------------------------------------------------------
// Types and where a model runs
// ---------------------------------------------------------------------------

export type TypeGroup = "chat" | "image" | "video" | "audio" | "embedding" | "search";

export const TYPE_GROUPS: { value: TypeGroup; label: string }[] = [
  { value: "chat", label: "Chat" },
  { value: "image", label: "Image" },
  { value: "video", label: "Video" },
  { value: "audio", label: "Audio" },
  { value: "embedding", label: "Embed" },
  { value: "search", label: "Search" },
];

export function typeGroup(type: string): TypeGroup {
  if (type === "audio_transcription" || type === "audio_speech") return "audio";
  if (type === "image" || type === "video" || type === "embedding" || type === "search") return type;
  return "chat";
}

export const MODEL_TYPE_NAMES: Record<string, string> = {
  chat: "Chat",
  embedding: "Embeddings",
  audio_transcription: "Speech to text",
  audio_speech: "Text to speech",
  image: "Image",
  video: "Video",
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

export function money(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return "$0";
  if (value >= 100) return `$${Math.round(value)}`;
  if (value >= 1) return `$${+value.toFixed(2)}`;
  if (value >= 0.01) return `$${+value.toFixed(3)}`;
  return `$${+value.toPrecision(2)}`;
}

/** The model's price in the unit its type bills by, or null when none is set. */
export function priceLabel(model: Pick<ModelRoute, "model_type" | "input_cost_per_token" | "output_cost_per_token" | "cost_per_image" | "cost_per_video" | "cost_per_character" | "cost_per_audio_second">): string | null {
  const perM = (v: number) => money(v * 1_000_000);
  switch (model.model_type) {
    case "image":
      return model.cost_per_image > 0 ? `${money(model.cost_per_image)} / image` : null;
    case "video":
      return model.cost_per_video > 0 ? `${money(model.cost_per_video)} / video` : null;
    case "audio_speech":
      return model.cost_per_character > 0 ? `${perM(model.cost_per_character)} / 1M chars` : null;
    case "audio_transcription":
      return model.cost_per_audio_second > 0 ? `${money(model.cost_per_audio_second * 60)} / min` : null;
    case "embedding":
      return model.input_cost_per_token > 0 ? `${perM(model.input_cost_per_token)} / 1M` : null;
    case "search":
      return null;
    default:
      if (model.input_cost_per_token <= 0 && model.output_cost_per_token <= 0) return null;
      return `${perM(model.input_cost_per_token)} · ${perM(model.output_cost_per_token)}`;
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
    const pool = model.enabled ? poolOccupancy(model.model_name, model, fairshare, discovery) : { inFlight: 0, cap: 0, queued: 0 };
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

/** Name, alias, upstream, description or tag: every word of the query must match one of them. */
export function matchesQuery(model: ModelRoute, query: string): boolean {
  const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return true;
  const hay = [model.model_name, model.upstream_model, model.description, model.api_base, ...(model.aliases ?? []), ...(model.tags ?? []).map((t) => parseTagLevel(t).base)]
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
  const counts = { all: rows.length, chat: 0, image: 0, video: 0, audio: 0, embedding: 0, search: 0 };
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
  { id: "set-quantization", label: "Quantization", section: "general", keywords: "format fp8 bf16 mxfp4 precision" },
  { id: "set-upstream", label: "Upstream model", section: "connection", keywords: "native name served model" },
  { id: "set-api-base", label: "API base URL", section: "connection", keywords: "url endpoint address host" },
  { id: "set-api-key", label: "Upstream API key", section: "connection", keywords: "secret token auth bearer" },
  { id: "set-headers", label: "Upstream headers", section: "connection", keywords: "header routing hint" },
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
  { id: "set-cache", label: "Response cache", section: "capacity", keywords: "cache ttl hit" },
  { id: "set-autotune", label: "Auto-tune capacity", section: "capacity", keywords: "probe knee benchmark", types: TEXT },
  { id: "set-checks", label: "Health checks", section: "health", keywords: "interval probe scheduled alerts slack" },
  { id: "set-threshold", label: "Failure threshold", section: "health", keywords: "failures alert" },
  { id: "set-maintenance", label: "Maintenance window", section: "health", keywords: "maintenance pause note" },
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
export function fieldSection(name: string): "model" | "cache" | "reliability" | "health" | "capacity" | "knowledge" | null {
  if (name === "id" || name === "sections" || name.startsWith("has_") || name === "knowledge_loaded") return null;
  if (name === "cache_enabled" || name === "cache_ttl_secs") return "cache";
  if (["request_timeout_secs", "max_retries", "retry_backoff_ms", "endpoint_selection_mode", "debug_diagnostics"].includes(name)) return "reliability";
  if (["check_interval_secs", "failure_threshold", "maintenance_until", "maintenance_note", "checks_enabled", "alerts_enabled"].includes(name)) return "health";
  if (name === "max_in_flight" || name.startsWith("capacity_") || name === "per_replica_max_in_flight") return "capacity";
  if (name === "knowledge_collection") return "knowledge";
  return "model";
}

const FIELD_LABELS: [RegExp, string][] = [
  [/^description$/, "Description"],
  [/^model_type$/, "Model type"],
  [/^aliases$/, "Aliases"],
  [/^quantization$/, "Quantization"],
  [/^upstream_model$/, "Upstream model"],
  [/^api_base$/, "API base URL"],
  [/^api_key$/, "Upstream API key"],
  [/^upstream_headers$/, "Upstream headers"],
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
