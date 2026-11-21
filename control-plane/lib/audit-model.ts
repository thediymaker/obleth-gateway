import type { AuditEntry } from "@/lib/obleth";
import { dateLabel } from "@/lib/format";

/**
 * The Audit page's pure logic: each event as a sentence with the thing's
 * name, bursts folded into one line, and what a change changed.
 */

export type AuditKind = "models" | "deployments" | "keys" | "tenants" | "mcp" | "knowledge" | "settings" | "gateway";

export const KIND_LABEL: Record<AuditKind, string> = {
  models: "Models",
  deployments: "Deployments",
  keys: "Keys",
  tenants: "Tenants",
  mcp: "MCP servers",
  knowledge: "Knowledge",
  settings: "Settings",
  gateway: "The gateway",
};

/** Entity types behind each kind, as the gateway records them. */
export const KIND_TYPES: Record<AuditKind, string[]> = {
  models: ["model", "model_endpoint"],
  deployments: ["managed_model", "model_replica", "recipe"],
  keys: ["api_key"],
  tenants: ["tenant", "fairshare_group"],
  mcp: ["mcp_server"],
  knowledge: ["knowledge_collection", "knowledge_document"],
  settings: ["settings"],
  gateway: ["gateway", "usage"],
};

export function kindOf(entityType: string): AuditKind {
  for (const [k, types] of Object.entries(KIND_TYPES)) if (types.includes(entityType)) return k as AuditKind;
  return "gateway";
}

/** "admin token" for the shared token, the email for a person. */
export function actorLabel(actor: string): string {
  if (actor === "admin") return "admin token";
  if (actor === "identity-provisioning") return "identity sign-in";
  return actor;
}

export function isPerson(actor: string): boolean {
  return actor.includes("@");
}

function str(v: unknown): string | undefined {
  return typeof v === "string" && v.trim() ? v : undefined;
}

/** Names by entity id: today's things first, then names the log itself recorded (so deleted things keep theirs). */
export function buildNames(current: Record<string, string>, entries: AuditEntry[]): Record<string, string> {
  const names: Record<string, string> = {};
  for (const e of [...entries].reverse()) {
    const d = (e.detail ?? {}) as Record<string, unknown>;
    const n = str(d.name) ?? str(d.model_name) ?? str(d.title) ?? (e.entity_type === "api_key" ? str(d.prefix) : undefined);
    if (n) names[e.entity_id] = n;
  }
  return { ...names, ...current };
}

const SETTINGS_NAMES: Record<string, string> = {
  set_alert_settings: "alert settings",
  set_auto_router_settings: "auto router settings",
  set_boon_settings: "boon settings",
  set_charo_settings: "assistant settings",
  set_energy_settings: "energy settings",
  set_knowledge_settings: "retrieval settings",
  set_slurm_settings: "Slurm settings",
  set_usage_retention: "usage retention",
};

const SETTINGS_TABS: Record<string, string> = {
  set_alert_settings: "alerts",
  set_auto_router_settings: "routing",
  set_boon_settings: "boons",
  set_charo_settings: "assistant",
  set_energy_settings: "energy",
  set_usage_retention: "data",
};

/** Where an event's thing lives in the dashboard, or null when it has no page (or is gone). */
export function thingHref(e: Pick<AuditEntry, "entity_type" | "entity_id" | "action">, names: Record<string, string>, alive: Set<string>): string | null {
  const name = names[e.entity_id];
  if (e.entity_type === "settings") {
    if (e.action === "set_knowledge_settings") return "/knowledge?tab=retrieval";
    if (e.action === "set_slurm_settings") return "/deployments?slurm=1";
    return `/settings${SETTINGS_TABS[e.action] ? `?tab=${SETTINGS_TABS[e.action]}` : ""}`;
  }
  if (!alive.has(e.entity_id)) return null;
  switch (e.entity_type) {
    case "model":
      return name ? `/models/${encodeURIComponent(name)}` : null;
    case "managed_model":
      return name ? `/deployments/${encodeURIComponent(name)}` : null;
    case "tenant":
      return name ? `/tenants/${encodeURIComponent(name)}` : null;
    case "api_key":
      return `/keys?key=${e.entity_id}`;
    case "mcp_server":
      return "/mcp";
    case "knowledge_collection":
      return name ? `/knowledge/${encodeURIComponent(name)}` : null;
    case "fairshare_group":
      return "/fairshare/groups";
    default:
      return null;
  }
}

const THING: Record<string, string> = {
  model: "model",
  model_endpoint: "endpoint",
  managed_model: "deployment",
  model_replica: "replica",
  recipe: "recipe",
  api_key: "key",
  tenant: "tenant",
  fairshare_group: "group",
  mcp_server: "MCP server",
  knowledge_collection: "collection",
  knowledge_document: "document",
};

/** What a person did, as words before the thing's name: "added model", "turned off key". */
export function verbOf(e: Pick<AuditEntry, "action" | "entity_type">): string {
  const thing = THING[e.entity_type] ?? e.entity_type.replace(/_/g, " ");
  const a = e.action;
  if (e.entity_type === "settings") return "changed";
  if (a === "create_model") return "added model";
  if (a === "create_key") return "made key";
  if (a === "create_tenant") return "made tenant";
  if (a === "upload_knowledge_document") return "uploaded";
  if (a.startsWith("create_")) return `added ${thing}`;
  if (a.startsWith("delete_")) return `deleted ${thing}`;
  if (a.startsWith("update_") || a.startsWith("set_") || a === "put_managed_model") {
    const what = a.replace(/^(update|set)_/, "").replace(/^(model|tenant|key)_/, "").replace(/_/g, " ");
    return what === thing || what === "" || a === "put_managed_model" ? `changed ${thing}` : `changed the ${what} of ${thing}`;
  }
  if (a.startsWith("enable_")) return a.endsWith("_tracing") ? `turned on tracing for ${thing}` : `turned on ${thing}`;
  if (a.startsWith("disable_")) return a.endsWith("_tracing") ? `turned off tracing for ${thing}` : `turned off ${thing}`;
  if (a === "move_key") return "moved key";
  if (a.startsWith("reindex_")) return `reindexed ${thing}`;
  if (a === "restart_replica") return "restarted a replica of";
  if (a === "clear_lost_replicas") return "retried failed launches of";
  if (a === "activate_model") return "turned on model";
  if (a === "autotune_model" || a === "apply_autotune_capacity") return "tuned the capacity of model";
  return `${a.replace(/_/g, " ")}`;
}

/** The part after the name: "in service-accounts", "to heavy-users". */
export function tailOf(e: AuditEntry, names: Record<string, string>): string | null {
  const d = (e.detail ?? {}) as Record<string, unknown>;
  const tenant = str(d.tenant_id);
  if (e.action === "create_key" && tenant) return `in ${names[tenant] ?? "a deleted tenant"}`;
  if (e.action === "move_key" && tenant) return `to ${names[tenant] ?? "another tenant"}`;
  if (e.action === "set_tenant_allowlist" && Array.isArray(d.allowed_models)) return d.allowed_models.length ? `to ${d.allowed_models.slice(0, 3).join(", ")}${d.allowed_models.length > 3 ? ` and ${d.allowed_models.length - 3} more` : ""}` : "to every model";
  if (e.action === "set_tenant_status" && str(d.status)) return `to ${d.status}`;
  if (e.action === "upload_knowledge_document") {
    const c = str(d.collection_id);
    return c ? `to ${names[c] ?? "a collection"}` : null;
  }
  if (e.action === "delete_tenant" && typeof d.keys_removed === "number" && d.keys_removed) return `and its ${d.keys_removed} key${d.keys_removed === 1 ? "" : "s"}`;
  return null;
}

/** The thing's name as shown: a settings page's name, or the entity's. */
export function thingName(e: AuditEntry, names: Record<string, string>): string {
  if (e.entity_type === "settings") return SETTINGS_NAMES[e.action] ?? e.action.replace(/^set_/, "").replace(/_/g, " ");
  if (e.entity_type === "gateway" || e.entity_type === "usage") return "";
  return names[e.entity_id] ?? `${e.entity_id.slice(0, 8)}…`;
}

// ---------------------------------------------------------------------------
// Bursts
// ---------------------------------------------------------------------------

export interface AuditGroup {
  /** The newest event first; one event for most lines. */
  events: AuditEntry[];
  actor: string;
  action: string;
  entityType: string;
}

const BURST_MS = 60_000;

/**
 * Fold runs of the same change by the same person, each within a minute of
 * the next, into one line: "updated 30 models". Entries come newest first.
 */
export function groupBursts(entries: AuditEntry[]): AuditGroup[] {
  const out: AuditGroup[] = [];
  for (const e of entries) {
    const last = out[out.length - 1];
    const prev = last?.events[last.events.length - 1];
    if (last && prev && last.actor === e.actor && last.action === e.action && last.entityType === e.entity_type && Date.parse(prev.ts) - Date.parse(e.ts) <= BURST_MS) {
      last.events.push(e);
    } else {
      out.push({ events: [e], actor: e.actor, action: e.action, entityType: e.entity_type });
    }
  }
  return out;
}

const PAST: Record<string, string> = {
  create: "added",
  update: "changed",
  set: "changed",
  put: "changed",
  delete: "deleted",
  enable: "turned on",
  disable: "turned off",
  reindex: "reindexed",
  upload: "uploaded",
  restart: "restarted",
  move: "moved",
};

/**
 * One line for a group: "changed 30 models" with the first few names, or,
 * when every event is the same thing, that thing's verb and how many times.
 */
export function burstSummary(g: AuditGroup, names: Record<string, string>): { verb: string; things: string[]; more: number; sameThing: boolean; times: number } {
  const ids = [...new Set(g.events.map((e) => e.entity_id))];
  const sameThing = ids.length === 1;
  const first = g.events[0];
  let verb = verbOf(first);
  if (!sameThing) {
    const past = first.action === "create_key" || first.action === "create_tenant" ? "made" : PAST[first.action.split("_")[0]];
    const plural = `${THING[g.entityType] ?? g.entityType.replace(/_/g, " ")}s`;
    verb = past ? `${past}${first.action.endsWith("_tracing") ? " tracing for" : ""} ${ids.length} ${plural}` : `${verb} (${ids.length})`;
  }
  return { verb, things: ids.slice(0, 3).map((id) => names[id] ?? `${id.slice(0, 8)}…`), more: Math.max(0, ids.length - 3), sameThing, times: g.events.length };
}

// ---------------------------------------------------------------------------
// What changed
// ---------------------------------------------------------------------------

export interface FieldChange {
  field: string;
  before?: string;
  after: string;
  changed: boolean;
}

function show(v: unknown): string {
  if (v === null || v === undefined) return "none";
  if (typeof v === "string") return v === "" ? "blank" : v;
  if (typeof v === "number" || typeof v === "boolean") return String(v);
  const s = JSON.stringify(v);
  return s.length > 120 ? `${s.slice(0, 117)}…` : s;
}

const NOISE = new Set(["created_at", "updated_at", "id"]);

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/**
 * The field diff the gateway recorded with the event, when it did: an
 * `update_model` detail carries `changes: { field: { before, after } }`.
 */
export function recordedChanges(e: AuditEntry): FieldChange[] | null {
  const d = e.detail;
  if (!isRecord(d) || !isRecord(d.changes)) return null;
  return Object.entries(d.changes)
    .filter((entry): entry is [string, Record<string, unknown>] => isRecord(entry[1]) && "after" in entry[1])
    .map(([field, c]) => ({ field, before: show(c.before), after: show(c.after), changed: true }));
}

/**
 * An event's fields: the changes the gateway recorded with it, or else a
 * comparison with the same thing's previous event when its detail has the
 * same shape. Which values changed from what, then the rest.
 */
export function fieldChanges(e: AuditEntry, previous: AuditEntry | undefined): FieldChange[] {
  const d = (e.detail ?? {}) as Record<string, unknown>;
  if (typeof d !== "object" || Array.isArray(d)) return [{ field: "detail", after: show(d), changed: false }];
  const recorded = recordedChanges(e);
  if (recorded) {
    const rest = Object.entries(d)
      .filter(([k]) => k !== "changes" && !NOISE.has(k))
      .map(([k, v]) => ({ field: k, after: show(v), changed: false }));
    return [...recorded, ...rest];
  }
  const p = previous && typeof previous.detail === "object" && previous.detail && !Array.isArray(previous.detail) ? (previous.detail as Record<string, unknown>) : null;
  const rows = Object.entries(d)
    .filter(([k]) => !NOISE.has(k))
    .map(([k, v]) => {
      const before = p && k in p ? show(p[k]) : undefined;
      const after = show(v);
      return { field: k, before, after, changed: before !== undefined && before !== after };
    });
  return rows.sort((a, b) => Number(b.changed) - Number(a.changed));
}

/**
 * Each entry's previous change to the same thing (for settings, the same
 * kind of change), by entry id, in one pass: what a line compares with.
 * Entries without an earlier one in view are absent.
 */
export function previousEntries(entries: AuditEntry[]): Map<number, AuditEntry> {
  const thing = (e: AuditEntry) => `${e.entity_type}\u0000${e.entity_id}\u0000${e.entity_type === "settings" ? e.action : ""}`;
  const previous = new Map<number, AuditEntry>();
  const last = new Map<string, AuditEntry>();
  for (const e of [...entries].sort((a, b) => a.id - b.id)) {
    const before = last.get(thing(e));
    if (before) previous.set(e.id, before);
    last.set(thing(e), e);
  }
  return previous;
}

/** A one-line summary of what changed, for the event's line: "classifier_model router-classifier-v1 → router-classifier-v2". */
export function changeLine(e: AuditEntry, previous: AuditEntry | undefined): string | null {
  const changed = fieldChanges(e, previous).filter((c) => c.changed);
  if (changed.length === 0) return null;
  const first = changed.slice(0, 2).map((c) => `${c.field.replace(/_/g, " ")} ${c.before} → ${c.after}`).join(", ");
  return changed.length > 2 ? `${first} and ${changed.length - 2} more` : first;
}

// ---------------------------------------------------------------------------
// Filters and ranges
// ---------------------------------------------------------------------------

export type AuditRange = "24h" | "7d" | "30d" | "all";

export const RANGE_LABEL: Record<AuditRange, string> = { "24h": "24 hours", "7d": "7 days", "30d": "30 days", all: "All" };

export function rangeSince(range: AuditRange, now = Date.now()): string | undefined {
  const ms = range === "24h" ? 86_400_000 : range === "7d" ? 7 * 86_400_000 : range === "30d" ? 30 * 86_400_000 : 0;
  return ms ? new Date(now - ms).toISOString() : undefined;
}

export interface AuditFilters {
  range: AuditRange;
  who: string;
  kind: AuditKind | "";
  q: string;
  /** `entity_type:entity_id`, from a thing's own "Audit log" link. */
  entity: string;
}

export function filtersFromParams(p: Record<string, string | undefined>): AuditFilters {
  const range = (["24h", "7d", "30d", "all"] as const).find((r) => r === p.range) ?? (p.entity ? "all" : "7d");
  const kind = (Object.keys(KIND_LABEL) as AuditKind[]).find((k) => k === p.kind) ?? "";
  return { range, who: p.who ?? "", kind, q: p.q ?? "", entity: p.entity ?? "" };
}

/** Entries the gateway returns per read; a full page means there may be older ones. */
export const AUDIT_PAGE = 500;

/** The gateway query for these filters. A kind sends all its entity types, so the gateway pages over that kind only. */
export function queryFor(f: AuditFilters, now = Date.now()) {
  const [entityType, entityId] = f.entity.includes(":") ? f.entity.split(":", 2) : ["", ""];
  return {
    actor: f.who || undefined,
    entityType: entityType || (f.kind ? KIND_TYPES[f.kind].join(",") : undefined),
    entityId: entityId || undefined,
    since: rangeSince(f.range, now),
    q: f.q.trim() || undefined,
  };
}

export function paramsFor(f: AuditFilters): string {
  const p = new URLSearchParams();
  if (f.range !== "7d") p.set("range", f.range);
  if (f.who) p.set("who", f.who);
  if (f.kind) p.set("kind", f.kind);
  if (f.q.trim()) p.set("q", f.q.trim());
  if (f.entity) p.set("entity", f.entity);
  const s = p.toString();
  return s ? `?${s}` : "";
}

/** The audit log filtered to one thing: what its page's "Audit log" link opens. */
export function auditHref(entityType: string, entityId: string): string {
  return `/audit?entity=${encodeURIComponent(`${entityType}:${entityId}`)}`;
}

/** "Sep 25", "Today", "Yesterday", in the reader's time. */
export function dayLabel(ts: string, now = new Date()): string {
  const d = new Date(ts);
  const same = (a: Date, b: Date) => a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (same(d, now)) return "Today";
  if (same(d, yesterday)) return "Yesterday";
  return dateLabel(d, { year: d.getFullYear() !== now.getFullYear() });
}

export function toCsv(entries: AuditEntry[], names: Record<string, string>): string {
  const esc = (v: string) => (/[",\n]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v);
  const head = "id,time,actor,action,kind,entity_type,entity_id,name,detail";
  return [head, ...entries.map((e) => [String(e.id), e.ts, e.actor, e.action, KIND_LABEL[kindOf(e.entity_type)], e.entity_type, e.entity_id, names[e.entity_id] ?? "", JSON.stringify(e.detail ?? {})].map(esc).join(","))].join("\n");
}
