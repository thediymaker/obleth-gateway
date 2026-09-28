import type {
  AuditEntry,
  CapacityDiscoveryView,
  FairshareLiveView,
  ModelHealthSummary,
  ModelRoute,
  UsageModelAgg,
  UsageTimePoint,
} from "@/lib/obleth";
import type { OverviewSummary } from "@/lib/overview-summary";
import { isWaitingBelowShare } from "@/lib/fairshare";
import { modelHref } from "@/lib/model-links";

/** The Overview's one time control. Every tile, chart and table follows it. */
export type OverviewRange = "1h" | "24h" | "7d";

export const RANGE_MS: Record<OverviewRange, number> = { "1h": 3_600_000, "24h": 86_400_000, "7d": 604_800_000 };
/** Bucket widths that divide each range evenly, so the previous window lines up bucket for bucket. */
export const RANGE_BUCKET_MS: Record<OverviewRange, number> = { "1h": 60_000, "24h": 1_800_000, "7d": 21_600_000 };
export const RANGE_LABEL: Record<OverviewRange, string> = { "1h": "Last hour", "24h": "Last 24 hours", "7d": "Last 7 days" };
export const PREVIOUS_LABEL: Record<OverviewRange, string> = { "1h": "the hour before", "24h": "yesterday", "7d": "the week before" };
export const BUCKET_LABEL: Record<OverviewRange, string> = { "1h": "Every minute", "24h": "Every 30 minutes", "7d": "Every 6 hours" };

export function isOverviewRange(value: unknown): value is OverviewRange {
  return value === "1h" || value === "24h" || value === "7d";
}

export interface SeriesPoint {
  t: number;
  requests: number;
  tokens: number;
  errors: number;
  cost: number;
  /** Median time to first token in the bucket, or null when nothing produced a first token. */
  ttft: number | null;
  prevRequests: number;
  prevTokens: number;
  prevErrors: number;
  prevTtft: number | null;
}

/**
 * Lay one series read (covering two windows) out as the current window's
 * buckets, each carrying the matching bucket one window earlier. Missing
 * buckets are zero, not skipped, so gaps read as quiet time.
 */
export function buildSeries(points: UsageTimePoint[], range: OverviewRange, now: number): SeriesPoint[] {
  const span = RANGE_MS[range];
  const bucket = RANGE_BUCKET_MS[range];
  const byBucket = new Map(points.map((p) => [Number(p.bucket_ms), p]));
  const last = Math.floor(now / bucket) * bucket;
  const first = last - span + bucket;
  const out: SeriesPoint[] = [];
  for (let t = first; t <= last; t += bucket) {
    const cur = byBucket.get(t);
    const prev = byBucket.get(t - span);
    out.push({
      t,
      requests: Number(cur?.requests ?? 0),
      tokens: Number(cur?.total_tokens ?? 0),
      errors: Number(cur?.errors ?? 0),
      cost: Number(cur?.cost_usd ?? 0),
      ttft: cur?.p50_ttft_ms ? Number(cur.p50_ttft_ms) : null,
      prevRequests: Number(prev?.requests ?? 0),
      prevTokens: Number(prev?.total_tokens ?? 0),
      prevErrors: Number(prev?.errors ?? 0),
      prevTtft: prev?.p50_ttft_ms ? Number(prev.p50_ttft_ms) : null,
    });
  }
  return out;
}

/** Additive totals for the window before the current one: (both windows) − (current window). */
export function previousTotals(current: OverviewSummary, both: OverviewSummary) {
  const minus = (a: number, b: number) => Math.max(0, a - b);
  return {
    requests: minus(both.requests, current.requests),
    tokens: minus(both.tokens, current.tokens),
    errors: minus(both.errors, current.errors),
    cost: minus(both.cost, current.cost),
  };
}

/** "▲ 11% vs yesterday"; null when there is nothing to compare with. */
export function changeLabel(current: number, previous: number, against: string): string | null {
  if (!previous) return null;
  const pct = ((current - previous) / previous) * 100;
  if (Math.abs(pct) < 0.5) return `Same as ${against}`;
  return `${pct > 0 ? "▲" : "▼"} ${Math.abs(pct) >= 10 ? Math.round(Math.abs(pct)) : Math.abs(pct).toFixed(1)}% vs ${against}`;
}

export type HealthState = "healthy" | "unhealthy" | "unknown" | "maintenance";

export function healthState(row: ModelHealthSummary | undefined, now = Date.now()): HealthState {
  if (!row) return "unknown";
  if (row.maintenance_until && new Date(row.maintenance_until).getTime() > now) return "maintenance";
  if (row.status === "healthy" || row.status === "unhealthy") return row.status;
  return "unknown";
}

export function isBenchmarkRoute(model: Pick<ModelRoute, "model_name" | "upstream_model" | "api_base">) {
  const s = [model.model_name, model.upstream_model, model.api_base].join(" ").toLowerCase();
  return s.includes("benchmark") || s.includes("mock-model") || s.includes("mock-backend");
}

export interface FleetTile {
  id: string;
  name: string;
  type: string;
  health: HealthState;
  healthRow?: ModelHealthSummary;
  inFlight: number;
  cap: number;
  queued: number;
  requests: number;
  p50TtftMs: number;
  /** A condition that belongs in Needs attention. */
  attention: "down" | "full" | null;
}

/**
 * Live pool occupancy for one model: cluster-wide in shared mode, else this
 * gateway's own. The fairshare snapshot lists only the pools the answering
 * gateway has served since it started, so a model it has not seen falls back
 * to capacity discovery (its enforced size and cluster-wide count), then to
 * the configured size.
 */
export function poolOccupancy(name: string, model: ModelRoute, fairshare?: FairshareLiveView, discovery?: CapacityDiscoveryView) {
  const shared = fairshare?.mode === "shared";
  const pool = fairshare?.pools?.find((p) => p.model === name);
  if (pool) {
    return {
      inFlight: shared ? pool.cluster_in_flight ?? pool.in_flight : pool.in_flight,
      cap: shared ? pool.configured_cap ?? pool.cap : pool.cap,
      queued: pool.queued,
    };
  }
  const found = discovery?.models.find((d) => d.model_name === name && d.enforced_max_in_flight > 0);
  if (found) {
    return {
      inFlight: found.cluster_in_flight ?? found.in_flight,
      cap: found.enforced_max_in_flight,
      queued: fairshare?.model_queued?.[name] ?? 0,
    };
  }
  return {
    inFlight: fairshare?.model_in_flight?.[name] ?? 0,
    cap: model.max_in_flight ?? fairshare?.default_model_max_in_flight ?? 0,
    queued: fairshare?.model_queued?.[name] ?? 0,
  };
}

const TILE_ORDER: Record<HealthState, number> = { unhealthy: 0, healthy: 1, unknown: 2, maintenance: 3 };

/**
 * Every enabled model as a fleet tile, problems first (down, then full),
 * then by how busy it is right now, then by traffic in the window.
 */
export function buildFleet(
  models: ModelRoute[],
  health: ModelHealthSummary[],
  fairshare: FairshareLiveView | undefined,
  usage: UsageModelAgg[],
  now = Date.now(),
  discovery?: CapacityDiscoveryView,
): FleetTile[] {
  const healthById = new Map(health.map((h) => [h.model_id, h]));
  const usageByName = new Map(usage.map((u) => [u.model, u]));
  const tiles = models
    .filter((m) => m.enabled && !isBenchmarkRoute(m))
    .map<FleetTile>((m) => {
      const row = healthById.get(m.id);
      const state = healthState(row, now);
      const pool = poolOccupancy(m.model_name, m, fairshare, discovery);
      const u = usageByName.get(m.model_name);
      const full = pool.queued > 0 && pool.cap > 0 && pool.inFlight >= pool.cap;
      return {
        id: m.id,
        name: m.model_name,
        type: m.model_type,
        health: state,
        healthRow: row,
        ...pool,
        requests: Number(u?.requests ?? 0),
        p50TtftMs: Number(u?.p50_ttft_ms ?? 0),
        attention: state === "unhealthy" ? "down" : full ? "full" : null,
      };
    });
  const load = (t: FleetTile) => (t.cap > 0 ? t.inFlight / t.cap : 0);
  return tiles.sort((a, b) =>
    Number(b.attention === "down") - Number(a.attention === "down") ||
    Number(b.attention === "full") - Number(a.attention === "full") ||
    TILE_ORDER[a.health] - TILE_ORDER[b.health] ||
    load(b) - load(a) ||
    b.requests - a.requests ||
    a.name.localeCompare(b.name),
  );
}

/** Friendly names for the model types a registry uses, for the fleet's filter chips. */
export function modelTypeLabel(type: string): string {
  const labels: Record<string, string> = {
    chat: "Chat", completion: "Chat", image: "Image", video: "Video", embedding: "Embed", embeddings: "Embed",
    rerank: "Rerank", audio_transcription: "Audio", audio_speech: "Audio", transcription: "Audio", speech: "Audio", tts: "Audio",
  };
  return labels[type] ?? type.replace(/[_-]+/g, " ").replace(/^\w/, (c) => c.toUpperCase());
}

export interface AttentionItem {
  /** Stable across polls, so a hidden item stays hidden. */
  key: string;
  badge: string;
  /** The inverted badge marks the conditions breaking requests now. */
  urgent: boolean;
  title: string;
  detail: string;
  actions: { label: string; href: string; primary?: boolean }[];
}

const plural = (n: number, one: string, many = `${one}s`) => `${n.toLocaleString()} ${n === 1 ? one : many}`;

function ago(iso: string | null | undefined, now: number): string | null {
  if (!iso) return null;
  const s = Math.max(0, Math.round((now - new Date(iso).getTime()) / 1000));
  if (!Number.isFinite(s)) return null;
  if (s < 90) return `${s} s ago`;
  if (s < 5400) return `${Math.round(s / 60)} min ago`;
  return `${Math.round(s / 3600)} h ago`;
}

/**
 * What the Overview says needs a person, most urgent first: failing models,
 * full pools that are queuing, then tenants waiting below their fair share.
 * `watching` holds the quieter conditions shown on one line underneath.
 */
export function buildAttention(fleet: FleetTile[], fairshare: FairshareLiveView | undefined, now = Date.now()) {
  const items: AttentionItem[] = [];
  for (const t of fleet.filter((f) => f.attention === "down")) {
    const h = t.healthRow;
    const parts = [
      h?.consecutive_failures ? `${plural(h.consecutive_failures, "failed check")} in a row` : "Failing its health check",
      h?.last_http_status ? `last check HTTP ${h.last_http_status}${h.last_message ? ` “${h.last_message.slice(0, 80)}”` : ""}` : h?.last_message ? `“${h.last_message.slice(0, 80)}”` : null,
      ago(h?.last_checked_at, now),
    ].filter(Boolean);
    items.push({
      key: `down:${t.name}`,
      badge: "Down",
      urgent: true,
      title: `${t.name} is failing health checks`,
      detail: parts.join(" · "),
      actions: [{ label: "Open model", href: modelHref(t.name), primary: true }, { label: "Request logs", href: `/logs?model=${encodeURIComponent(t.name)}&status=error` }],
    });
  }
  const full = fleet.filter((f) => f.attention === "full");
  for (const t of full) {
    items.push({
      key: `full:${t.name}`,
      badge: "Full",
      urgent: true,
      title: `${t.name} is at capacity and queuing`,
      detail: `${t.inFlight.toLocaleString()} of ${t.cap.toLocaleString()} slots in use · ${t.queued.toLocaleString()} queued`,
      actions: [{ label: "Open the pool", href: `/fairshare?pool=${encodeURIComponent(t.name)}`, primary: true }, { label: "Capacity", href: modelHref(t.name, "capacity") }],
    });
  }
  const queued = fairshare?.global_queued ?? 0;
  if (queued > 0 && full.length === 0) {
    const waiting = fairshare?.tenants.filter((t) => t.queued > 0).length ?? 0;
    items.push({
      key: "queued",
      badge: "Queued",
      urgent: false,
      title: `${plural(queued, "request")} waiting for admission`,
      detail: `${plural(waiting, "tenant")} waiting`,
      actions: [{ label: "Open Fairshare", href: "/fairshare", primary: true }],
    });
  }
  for (const t of (fairshare?.tenants ?? []).filter(isWaitingBelowShare)) {
    items.push({
      key: `share:${t.tenant_id}`,
      badge: "Below share",
      urgent: false,
      title: `${t.name || "A tenant"} is waiting below its fair share`,
      detail: `${plural(t.queued, "request")} queued while it holds ${plural(t.in_flight, "slot")} against an expected ${Math.floor(t.expected_slots)}`,
      actions: [{ label: "Open Fairshare", href: "/fairshare" }],
    });
  }
  const watching = [
    ...fleet.filter((f) => f.health === "unknown").map((f) => `${f.name} has no health data yet`),
    ...fleet.filter((f) => f.health === "maintenance").map((f) => {
      const until = f.healthRow?.maintenance_until ? new Date(f.healthRow.maintenance_until).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) : null;
      return `${f.name} is in maintenance${until ? ` until ${until}` : ""}`;
    }),
  ];
  return { items, watching };
}

/** One audit entry as a short sentence: who did what to which thing. */
export function describeAudit(entry: AuditEntry, names: { models: Map<string, string>; tenants: Map<string, string> }) {
  const detail = entry.detail && typeof entry.detail === "object" && !Array.isArray(entry.detail) ? (entry.detail as Record<string, unknown>) : {};
  const fromDetail = [detail.name, detail.model_name, detail.tenant_name].find((v): v is string => typeof v === "string" && !!v.trim());
  const named = entry.entity_type === "model" ? names.models.get(entry.entity_id) : entry.entity_type === "tenant" ? names.tenants.get(entry.entity_id) : undefined;
  const entity = entry.entity_type.replace(/[_-]+/g, " ");
  const target = named ?? fromDetail ?? (entry.entity_id ? `${entity} ${entry.entity_id.length > 12 ? entry.entity_id.slice(0, 8) : entry.entity_id}` : entity);
  const action = entry.action.replace(/[_-]+/g, " ").trim().toLowerCase();
  return { actor: entry.actor || "system", action, target };
}

export function compact(n: number): string {
  if (!Number.isFinite(n)) return "0";
  const abs = Math.abs(n);
  if (abs >= 1e9) return `${+(n / 1e9).toFixed(abs >= 1e10 ? 0 : 1)}B`;
  if (abs >= 1e6) return `${+(n / 1e6).toFixed(abs >= 1e7 ? 0 : 1)}M`;
  if (abs >= 1e3) return `${+(n / 1e3).toFixed(abs >= 1e4 ? 0 : 1)}K`;
  return String(Math.round(n));
}

export function formatMs(ms: number): string {
  if (!ms || !Number.isFinite(ms)) return "—";
  if (ms >= 60_000) return `${+(ms / 60_000).toFixed(1)} min`;
  if (ms >= 1000) return `${+(ms / 1000).toFixed(ms >= 10_000 ? 0 : 1)} s`;
  return `${Math.round(ms)} ms`;
}
