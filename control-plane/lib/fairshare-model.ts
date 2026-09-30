import type {
  CapacityDiscoveryView,
  FairshareHistoryPoint,
  FairshareHistoryView,
  FairshareLiveView,
  KeyFairshareView,
  ModelRoute,
  TenantFairshareView,
} from "@/lib/obleth";
import { isWaitingBelowShare } from "@/lib/fairshare";
import { formatNumber } from "@/lib/utils";
import { isBenchmarkRoute, poolOccupancy } from "@/lib/overview-model";

// ---------------------------------------------------------------------------
// Scope and gateway modes
// ---------------------------------------------------------------------------

/** Present one pool with the same shape as the all-models view so every panel
 *  can render either without knowing which it got. */
export function scopedView(view: FairshareLiveView | undefined, model: string): FairshareLiveView | undefined {
  if (!view || model === "all") return view;
  const pool = view.pools?.find((p) => p.model === model);
  if (!pool) return view;
  return {
    ...view,
    max_in_flight: pool.cap,
    global_in_flight: pool.in_flight,
    global_queued: pool.queued,
    global_borrowed: pool.borrowed,
    groups: pool.groups,
    tenants: pool.tenants,
    keys: pool.keys,
    model_in_flight: { [pool.model]: pool.in_flight },
    model_queued: { [pool.model]: pool.queued },
  };
}

/** A gateway's share of a cluster-wide limit in split mode, as the gateway computes it. */
export function replicaShare(configured: number, replicas: number): number {
  return Math.max(Math.ceil(configured / Math.max(replicas, 1)), 1);
}

/** What the answering gateway divides the configured limits by in its current mode. */
export function limitDivisor(view: FairshareLiveView): number {
  const n = Math.max(view.replicas ?? 1, 1);
  switch (view.mode) {
    case "split":
      return n;
    case "fallback":
      return view.replica_aware === false ? 1 : n;
    case "shared":
    case "local":
      return 1;
    default:
      return view.replica_aware ? n : 1;
  }
}

/** Short label for the header badge; empty for a single gateway. */
export function slotModeBadge(view: FairshareLiveView): string {
  const n = Math.max(view.replicas ?? 1, 1);
  switch (view.mode) {
    case "shared":
      return `Shared slots · ${formatNumber(n)} gateways`;
    case "fallback":
      return view.replica_aware === false ? "Fallback · per gateway" : "Fallback · split";
    case "split":
      return `Split across ${formatNumber(n)} gateways`;
    default:
      return "";
  }
}

/** One line saying whose numbers the page shows and how the limits hold across gateways. */
export function limitsNote(view: FairshareLiveView): string {
  const n = Math.max(view.replicas ?? 1, 1);
  const here = `this gateway ${formatNumber(view.global_in_flight)}`;
  switch (view.mode) {
    case "shared": {
      const configured = view.configured_max_in_flight ?? view.max_in_flight;
      const cluster = view.cluster_in_flight == null ? "—" : formatNumber(view.cluster_in_flight);
      return `In flight ${cluster} of ${formatNumber(configured)} (cluster-wide, ${formatNumber(n)} gateways) · ${here}. Any gateway can use a model's whole pool; pool sizes, tenant and key caps and the ceiling hold across all of them. Queues and fairness below are this gateway's.`;
    }
    case "fallback":
      return view.replica_aware === false
        ? `Fallback: shared slots are unavailable, so each of ${formatNumber(n)} gateways enforces the full configured limits until Redis answers again. Counts are this gateway's own.`
        : `Fallback: shared slots are unavailable, so each of ${formatNumber(n)} gateways enforces ceil(configured / ${formatNumber(n)}) of every limit until Redis answers again. Counts are this gateway's own.`;
    case "split":
      return `Shared slots are off: each of ${formatNumber(n)} live gateways enforces ceil(configured / ${formatNumber(n)}) of every limit. Counts are this gateway's own.`;
    case "local":
      return n > 1
        ? `Shared slots and replica-aware sizing are off: each of ${formatNumber(n)} gateways enforces the full configured limits. Counts are this gateway's own.`
        : "1 live gateway: pools, caps and the ceiling are enforced at their configured size.";
    default:
      return "";
  }
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

export interface GroupHistoryPoint {
  time: string;
  ts: number;
  queued: number;
  [key: string]: number | string;
}

export interface GroupKey {
  name: string;
  key: string;
}

/** Cache of formatted clock times keyed by `ts_ms`, so re-running
 *  `buildHistoryChart` over an appended history only formats the new points
 *  instead of the whole retained window every poll. Bounded so a long-lived
 *  tab does not grow this without limit. */
const TIME_LABEL_CACHE_MAX = 4096;
const timeLabelCache = new Map<number, string>();

function formatPointTime(tsMs: number): string {
  let label = timeLabelCache.get(tsMs);
  if (label === undefined) {
    if (timeLabelCache.size >= TIME_LABEL_CACHE_MAX) timeLabelCache.clear();
    label = new Date(tsMs).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
    timeLabelCache.set(tsMs, label);
  }
  return label;
}

export const MAX_CHART_ROWS = 600;

/** Downsample `rows` to at most `max` entries, always keeping the last row so
 *  the chart's right edge stays current. Returns `rows` itself, unchanged,
 *  when it is already within budget. */
export function thinHistory<T>(rows: T[], max: number): T[] {
  if (rows.length <= max) return rows;
  const step = Math.ceil(rows.length / max);
  const thinned: T[] = [];
  for (let i = 0; i < rows.length; i += step) thinned.push(rows[i]);
  const last = rows[rows.length - 1];
  if (thinned[thinned.length - 1] !== last) {
    // Swap in the last row rather than appending when we are already at
    // budget, so the result never exceeds `max`.
    if (thinned.length >= max) thinned[thinned.length - 1] = last;
    else thinned.push(last);
  }
  return thinned;
}

function historyQs(params: { model?: string; since_ms?: number }) {
  const q = new URLSearchParams();
  if (params.model) q.set("model", params.model);
  if (params.since_ms !== undefined) q.set("since_ms", String(params.since_ms));
  const s = q.toString();
  return s ? `?${s}` : "";
}

export async function fetchHistory(model: string | undefined, sinceMs?: number) {
  const res = await fetch(`/api/live/fairshare/history${historyQs({ model, since_ms: sinceMs })}`);
  if (!res.ok) throw new Error("fairshare history unavailable");
  return (await res.json()) as FairshareHistoryView;
}

/** Merge tail points onto `prev`: only strictly newer timestamps are added,
 *  and points older than `retentionMs` before the newest are dropped. Returns
 *  `prev` itself when nothing changes. */
export function appendHistoryTail(
  prev: FairshareHistoryPoint[],
  fresh: FairshareHistoryPoint[],
  retentionMs: number,
): FairshareHistoryPoint[] {
  const newest = prev.length ? prev[prev.length - 1].ts_ms : -Infinity;
  const added = fresh.filter((p) => p.ts_ms > newest);
  if (added.length === 0) return prev;
  const merged = [...prev, ...added];
  const cutoff = retentionMs > 0 ? merged[merged.length - 1].ts_ms - retentionMs : -Infinity;
  return merged.filter((p) => p.ts_ms >= cutoff);
}

export function groupDataKey(name: string) {
  return `group:${name}`;
}

/** Project server points into the stacked chart's rows and the sorted group
 *  series. Every row carries every group so the stack never has holes. */
export function buildHistoryChart(points: FairshareHistoryPoint[]): {
  history: GroupHistoryPoint[];
  groupKeys: GroupKey[];
} {
  const names = new Set<string>();
  for (const p of points) for (const g of Object.keys(p.groups)) names.add(g);
  const groupKeys: GroupKey[] = [...names].sort((a, b) => a.localeCompare(b)).map((name) => ({ name, key: groupDataKey(name) }));
  const history = points.map((p) => {
    const row: GroupHistoryPoint = { time: formatPointTime(p.ts_ms), ts: p.ts_ms, queued: p.queued };
    for (const g of groupKeys) row[g.key] = p.groups[g.name] ?? 0;
    return row;
  });
  return { history, groupKeys };
}

/**
 * Monochrome tones for group series, lightest first, stepped in lightness so
 * they stay apart without hue. More groups than tones fold into "Other".
 */
export const GROUP_TONES = ["hsl(240 5% 84%)", "hsl(240 4% 58%)", "hsl(240 4% 42%)", "hsl(240 5% 70%)"];
export const OTHER_TONE = "hsl(240 4% 30%)";

// ---------------------------------------------------------------------------
// Pools
// ---------------------------------------------------------------------------

export type PoolState = "full" | "busy" | "normal" | "idle";

export interface PoolRow {
  model: string;
  inFlight: number;
  cap: number;
  queued: number;
  /** Tenants with work running or waiting in the pool, per the answering gateway's snapshot. */
  tenants: number;
  state: PoolState;
}

export function poolState(inFlight: number, cap: number, queued: number): PoolState {
  if (queued > 0 && cap > 0 && inFlight >= cap) return "full";
  if (cap > 0 && inFlight / cap >= 0.8) return "busy";
  return inFlight > 0 || queued > 0 ? "normal" : "idle";
}

/**
 * Every enabled model's pool, fullest first. Occupancy comes from the
 * snapshot's pool where the answering gateway has one, else capacity
 * discovery, else the configured size — the same reading as the Overview.
 */
export function buildPoolRows(models: ModelRoute[], view: FairshareLiveView | undefined, discovery?: CapacityDiscoveryView): PoolRow[] {
  const byName = new Map((view?.pools ?? []).map((p) => [p.model, p]));
  const rows = models
    .filter((m) => m.enabled && !isBenchmarkRoute(m))
    .map<PoolRow>((m) => {
      const occ = poolOccupancy(m.model_name, m, view, discovery);
      const pool = byName.get(m.model_name);
      const tenants = pool?.tenants.filter((t) => t.in_flight > 0 || t.queued > 0).length ?? 0;
      return { model: m.model_name, ...occ, tenants, state: poolState(occ.inFlight, occ.cap, occ.queued) };
    });
  const order: Record<PoolState, number> = { full: 0, busy: 1, normal: 2, idle: 3 };
  const ratio = (r: PoolRow) => (r.cap > 0 ? r.inFlight / r.cap : 0);
  return rows.sort((a, b) => order[a.state] - order[b.state] || ratio(b) - ratio(a) || b.inFlight - a.inFlight || a.model.localeCompare(b.model));
}

// ---------------------------------------------------------------------------
// Tenants inside a pool
// ---------------------------------------------------------------------------

/**
 * Where a tenant stands in a pool: waiting below its share (next in line),
 * waiting at or above it, holding more than its share, or within it.
 */
export type Standing = "next" | "waiting" | "above" | "within";

export interface PoolTenantRow {
  tenant: TenantFairshareView;
  standing: Standing;
  /** Slots held above the fair share, whole slots. */
  above: number;
  /** 1-based place among waiting tenants, in the order freed slots go to them. */
  place: number | null;
}

/** Waiting tenants in the order freed slots reach them: least served against weight first. */
export function nextInLine(tenants: TenantFairshareView[]): TenantFairshareView[] {
  return tenants.filter((t) => t.queued > 0).sort((a, b) => a.share_score - b.share_score || b.queued - a.queued);
}

export function poolTenantRows(tenants: TenantFairshareView[]): PoolTenantRow[] {
  const line = nextInLine(tenants);
  const place = new Map(line.map((t, i) => [t.tenant_id, i + 1]));
  const standingOf = (t: TenantFairshareView): Standing =>
    isWaitingBelowShare(t) ? "next" : t.queued > 0 ? "waiting" : t.in_flight - t.expected_slots >= 1 ? "above" : "within";
  const order: Record<Standing, number> = { next: 0, waiting: 1, above: 2, within: 3 };
  return tenants
    .filter((t) => t.in_flight > 0 || t.queued > 0)
    .map((t) => ({ tenant: t, standing: standingOf(t), above: Math.max(0, Math.floor(t.in_flight - t.expected_slots)), place: place.get(t.tenant_id) ?? null }))
    .sort((a, b) =>
      order[a.standing] - order[b.standing] ||
      (a.place ?? Infinity) - (b.place ?? Infinity) ||
      b.tenant.in_flight - a.tenant.in_flight ||
      a.tenant.name.localeCompare(b.tenant.name),
    );
}

/** The short story behind a waiting tenant, for its (?) tip. */
export function whyWaiting(row: PoolTenantRow, rows: PoolTenantRow[]) {
  const holders = rows.filter((r) => r.above > 0).sort((a, b) => b.above - a.above);
  const line = rows.filter((r) => r.place !== null).sort((a, b) => (a.place ?? 0) - (b.place ?? 0));
  return { holders, line, place: row.place };
}

// ---------------------------------------------------------------------------
// Weight previews
// ---------------------------------------------------------------------------

export interface WeightPreviewRow {
  model: string;
  cap: number;
  from: number;
  to: number;
  /** Other active tenants in the same group, who give up (or gain) the difference. */
  groupPeers: string[];
}

/**
 * What a tenant's fair share would be in each pool it is active in if its
 * weight changed, everything else held still. A pool's slots split between
 * its active groups by group weight, then between each group's active
 * tenants by tenant weight, so a tenant's weight only moves slots inside its
 * own group. `from` is the gateway's own figure; `to` scales it by the ratio
 * of the recomputed shares, so the preview never disagrees with the page
 * about where things stand now.
 */
export function previewTenantWeight(view: FairshareLiveView | undefined, tenantId: string, weight: number): WeightPreviewRow[] {
  const rows: WeightPreviewRow[] = [];
  for (const pool of view?.pools ?? []) {
    const me = pool.tenants.find((t) => t.tenant_id === tenantId);
    if (!me || (me.in_flight === 0 && me.queued === 0)) continue;
    const active = pool.tenants.filter((t) => t.in_flight > 0 || t.queued > 0 || t.tenant_id === tenantId);
    const peers = active.filter((t) => t.fairshare_group === me.fairshare_group && t.tenant_id !== tenantId);
    const groupTotal = (w: number) => peers.reduce((n, t) => n + t.weight, 0) + w;
    const before = me.weight / Math.max(groupTotal(me.weight), 1);
    const after = weight / Math.max(groupTotal(weight), 1);
    const cap = pool.configured_cap ?? pool.cap;
    rows.push({ model: pool.model, cap, from: me.expected_slots, to: before > 0 ? me.expected_slots * (after / before) : me.expected_slots, groupPeers: peers.map((p) => p.name) });
  }
  return rows.sort((a, b) => Math.abs(b.to - b.from) - Math.abs(a.to - a.from) || a.model.localeCompare(b.model));
}

export interface GroupShareRow {
  name: string;
  weight: number;
  active: boolean;
  /** Share of a full pool with every active group busy, 0–1. */
  share: number;
}

/**
 * Each group's share of a full pool: its weight over the weights of the
 * groups with work. With nothing active anywhere, every group counts, so the
 * page still says what the weights would mean.
 */
export function groupShares(groups: { name: string; weight: number; active: boolean }[]): GroupShareRow[] {
  const counted = groups.some((g) => g.active) ? groups.filter((g) => g.active) : groups;
  const total = counted.reduce((n, g) => n + g.weight, 0);
  return groups.map((g) => ({ ...g, share: counted.includes(g) && total > 0 ? g.weight / total : 0 }));
}

/** The keys to show under a tenant: that tenant's, least served against weight first. */
/**
 * How a key row is named. An end user of a key with per-end-user fairshare
 * reads as the key's name and the user the app named, since their row's id
 * is derived rather than a real key.
 */
export function keyRowLabel(k: KeyFairshareView): string {
  const name = k.name || k.key_id.slice(0, 8);
  return k.end_user ? `${name} · ${k.end_user}` : name;
}

export function tenantKeys(keys: KeyFairshareView[] | undefined, tenantId: string): KeyFairshareView[] {
  return (keys ?? []).filter((k) => k.tenant_id === tenantId).sort((a, b) => a.share_score - b.share_score);
}

/** Across pools: how many a tenant has work in, and the one it uses most. */
export function tenantPools(view: FairshareLiveView | undefined, tenantId: string) {
  const used = (view?.pools ?? [])
    .map((p) => ({ model: p.model, t: p.tenants.find((t) => t.tenant_id === tenantId) }))
    .filter((p): p is { model: string; t: TenantFairshareView } => !!p.t && (p.t.in_flight > 0 || p.t.queued > 0))
    .sort((a, b) => b.t.in_flight - a.t.in_flight || b.t.queued - a.t.queued);
  return { count: used.length, top: used[0]?.model ?? null };
}
