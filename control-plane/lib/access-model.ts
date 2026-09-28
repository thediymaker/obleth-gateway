import type { AdminUser } from "@/lib/auth/users";
import type { ApiKey, BudgetUsage, FairshareLiveView, KeyUsageSummary, Tenant, UsageAgg, WeeklyWindow } from "@/lib/obleth";

/**
 * The Tenants, API Keys and Users pages' pure logic: time zones and access
 * hours, budgets and their pace, rows, filters and sorting.
 */

// ---------------------------------------------------------------------------
// Time zones
// ---------------------------------------------------------------------------

/** The wall clock in `tz` at `at`, as its parts. */
function zonedParts(at: Date, tz: string) {
  const parts = new Intl.DateTimeFormat("en-US", {
    timeZone: tz,
    hourCycle: "h23",
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    weekday: "short",
  }).formatToParts(at);
  const get = (t: string) => parts.find((p) => p.type === t)?.value ?? "";
  return {
    year: Number(get("year")),
    month: Number(get("month")),
    day: Number(get("day")),
    hour: Number(get("hour")),
    minute: Number(get("minute")),
    second: Number(get("second")),
    weekday: ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"].indexOf(get("weekday")),
  };
}

export function isTimeZone(tz: string): boolean {
  try {
    new Intl.DateTimeFormat("en-US", { timeZone: tz });
    return true;
  } catch {
    return false;
  }
}

/** A `YYYY-MM-DDTHH:mm` wall-clock time in `tz` as a UTC ISO string. */
export function zonedToUtc(local: string, tz: string): string | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/.exec(local);
  if (!m) return null;
  const [y, mo, d, h, mi] = m.slice(1).map(Number);
  const guess = Date.UTC(y, mo - 1, d, h, mi);
  // The zone's offset at the guess, applied once and checked once more so a
  // time next to a DST change lands on the right side of it.
  let t = guess;
  for (let i = 0; i < 2; i++) {
    const p = zonedParts(new Date(t), tz);
    const shown = Date.UTC(p.year, p.month - 1, p.day, p.hour, p.minute, p.second);
    t += guess - shown;
  }
  return new Date(t).toISOString();
}

/** A UTC ISO time as `YYYY-MM-DDTHH:mm` on the wall clock in `tz`, for a datetime-local input. */
export function utcToZoned(iso: string | null, tz: string): string {
  if (!iso) return "";
  const at = new Date(iso);
  if (Number.isNaN(at.getTime())) return "";
  const p = zonedParts(at, tz);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${p.year}-${pad(p.month)}-${pad(p.day)}T${pad(p.hour)}:${pad(p.minute)}`;
}

/** "22:14" in `tz` now. */
export function clockIn(tz: string, now = new Date()): string {
  const p = zonedParts(now, tz);
  return `${String(p.hour).padStart(2, "0")}:${String(p.minute).padStart(2, "0")}`;
}

// ---------------------------------------------------------------------------
// Access hours
// ---------------------------------------------------------------------------

export type AccessState = "always" | "open" | "closed" | "not-yet" | "ended";

/** Whether a tenant's keys work at `now`: its dates first, then its weekly hours, in its own time zone. */
export function accessState(t: Pick<Tenant, "timezone" | "active_from" | "active_until" | "weekly_windows">, now = new Date()): AccessState {
  if (t.active_from && now < new Date(t.active_from)) return "not-yet";
  if (t.active_until && now >= new Date(t.active_until)) return "ended";
  const windows = t.weekly_windows ?? [];
  if (windows.length === 0) return t.active_from || t.active_until ? "open" : "always";
  const p = zonedParts(now, isTimeZone(t.timezone) ? t.timezone : "UTC");
  const minute = p.hour * 60 + p.minute;
  return windows.some((w) => w.day === p.weekday && minute >= w.start_min && minute < w.end_min) ? "open" : "closed";
}

const DAY_NAMES = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

function clock(min: number) {
  const h = Math.floor(min / 60);
  const m = min % 60;
  return `${h}:${String(m).padStart(2, "0")}`;
}

/** "Weekdays 7:00–22:00 · Sat 9:00–17:00"; days with the same hours are grouped. */
export function windowsSummary(windows: WeeklyWindow[] | null | undefined): string {
  if (!windows?.length) return "Open all hours";
  const byDay = new Map<number, string>();
  for (let d = 0; d < 7; d++) {
    const spans = windows.filter((w) => w.day === d).sort((a, b) => a.start_min - b.start_min).map((w) => `${clock(w.start_min)}–${clock(w.end_min)}`);
    if (spans.length) byDay.set(d, spans.join(", "));
  }
  const groups = new Map<string, number[]>();
  for (const [d, hours] of byDay) groups.set(hours, [...(groups.get(hours) ?? []), d]);
  return [...groups.entries()]
    .map(([hours, days]) => {
      const key = days.join(",");
      const label = key === "1,2,3,4,5" ? "Weekdays" : key === "0,6" ? "Weekends" : key === "0,1,2,3,4,5,6" ? "Every day" : days.map((d) => DAY_NAMES[d]).join(", ");
      return `${label} ${hours}`;
    })
    .join(" · ");
}

export function accessLabel(t: Pick<Tenant, "timezone" | "active_from" | "active_until" | "weekly_windows">, now = new Date()): string {
  const state = accessState(t, now);
  if (state === "always") return "Always";
  if (state === "not-yet") return `Opens ${new Date(t.active_from!).toLocaleDateString([], { month: "short", day: "numeric" })}`;
  if (state === "ended") return `Ended ${new Date(t.active_until!).toLocaleDateString([], { month: "short", day: "numeric" })}`;
  const hours = t.weekly_windows?.length ? windowsSummary(t.weekly_windows) : "Open";
  return `${hours} · ${state === "open" ? "open now" : "closed now"}`;
}

/** Half-hour cells per day in the week grid. */
export const GRID_SLOTS = 48;

/** A week as 7 rows (Sun=0) of half-hour cells, open or not. */
export function windowsToGrid(windows: WeeklyWindow[] | null | undefined): boolean[][] {
  const grid = Array.from({ length: 7 }, () => Array<boolean>(GRID_SLOTS).fill(false));
  for (const w of windows ?? []) {
    for (let s = 0; s < GRID_SLOTS; s++) {
      const start = s * 30;
      if (start < w.end_min && start + 30 > w.start_min) grid[w.day][s] = true;
    }
  }
  return grid;
}

/** The grid back as the gateway's windows: each run of open cells is one window. */
export function gridToWindows(grid: boolean[][]): WeeklyWindow[] {
  const out: WeeklyWindow[] = [];
  grid.forEach((row, day) => {
    let start = -1;
    for (let s = 0; s <= GRID_SLOTS; s++) {
      const on = s < GRID_SLOTS && row[s];
      if (on && start < 0) start = s;
      if (!on && start >= 0) {
        out.push({ day, start_min: start * 30, end_min: s * 30 });
        start = -1;
      }
    }
  });
  return out;
}

/** Whether every window starts and ends on a half hour, so the grid shows it exactly. */
export function windowsFitGrid(windows: WeeklyWindow[] | null | undefined): boolean {
  return (windows ?? []).every((w) => w.start_min % 30 === 0 && w.end_min % 30 === 0);
}

// ---------------------------------------------------------------------------
// Budgets
// ---------------------------------------------------------------------------

export interface BudgetView {
  /** The larger of the cost and token shares, 0 to 1 (can pass 1 briefly). */
  share: number;
  /** "$46 of $50 this month", "1.2M of 5M tokens this term". */
  label: string;
  /** For a monthly budget part-way through: where this pace lands at the reset. */
  pace: string | null;
  resetsAt: Date | null;
}

function money(v: number) {
  return v >= 100 ? `$${Math.round(v).toLocaleString()}` : `$${v.toFixed(v >= 10 ? 0 : 2)}`;
}

function tokensShort(n: number) {
  if (n >= 1e9) return `${+(n / 1e9).toFixed(1)}B`;
  if (n >= 1e6) return `${+(n / 1e6).toFixed(1)}M`;
  if (n >= 1e3) return `${+(n / 1e3).toFixed(1)}K`;
  return String(n);
}

const PERIOD_WORD: Record<string, string> = { monthly: "this month", term: "this term", lifetime: "so far" };

/** A capped budget's use in its own period, from the gateway's counters. */
export function budgetView(u: BudgetUsage | undefined, now = new Date()): BudgetView | null {
  if (!u || (u.budget_cost_usd == null && u.budget_tokens == null)) return null;
  const costShare = u.budget_cost_usd ? u.used_cost_usd / u.budget_cost_usd : 0;
  const tokenShare = u.budget_tokens ? u.used_tokens / u.budget_tokens : 0;
  const byCost = u.budget_cost_usd != null && (costShare >= tokenShare || u.budget_tokens == null);
  const label = byCost
    ? `${money(u.used_cost_usd)} of ${money(u.budget_cost_usd!)} ${PERIOD_WORD[u.period] ?? ""}`.trim()
    : `${tokensShort(u.used_tokens)} of ${tokensShort(u.budget_tokens!)} tokens ${PERIOD_WORD[u.period] ?? ""}`.trim();
  const resetsAt = u.resets_at ? new Date(u.resets_at) : null;
  let pace: string | null = null;
  if (u.period === "monthly" && u.period_start && resetsAt) {
    const start = new Date(u.period_start).getTime();
    const elapsed = (now.getTime() - start) / (resetsAt.getTime() - start);
    if (elapsed > 0.1 && elapsed < 1) {
      const projected = (byCost ? u.used_cost_usd : u.used_tokens) / elapsed;
      pace = `on pace for ${byCost ? money(projected) : `${tokensShort(Math.round(projected))} tokens`}`;
    }
  }
  return { share: Math.max(costShare, tokenShare), label, pace, resetsAt };
}

export const NEAR_BUDGET = 0.8;

// ---------------------------------------------------------------------------
// Tenants
// ---------------------------------------------------------------------------

export interface TenantRow {
  tenant: Tenant;
  keys: number;
  keysOff: number;
  users: number;
  requests24h: number;
  cost24h: number;
  budget: BudgetView | null;
  access: AccessState;
  /** In flight right now across its pools, and requests waiting. */
  running: number;
  waiting: number;
}

export function buildTenantRows(
  tenants: Tenant[],
  keys: Pick<ApiKey, "tenant_id" | "disabled">[],
  users: Pick<AdminUser, "tenantId">[],
  usage24h: UsageAgg[],
  budgets: BudgetUsage[],
  fairshare?: FairshareLiveView,
  now = new Date(),
): TenantRow[] {
  const use = new Map(usage24h.map((u) => [u.tenant_id, u]));
  const budget = new Map(budgets.filter((b) => b.scope === "tenant").map((b) => [b.id, b]));
  const live = new Map((fairshare?.tenants ?? []).map((t) => [t.tenant_id, t]));
  return tenants.map((t) => ({
    tenant: t,
    keys: keys.filter((k) => k.tenant_id === t.id).length,
    keysOff: keys.filter((k) => k.tenant_id === t.id && k.disabled).length,
    users: users.filter((u) => u.tenantId === t.id).length,
    requests24h: Number(use.get(t.id)?.requests ?? 0),
    cost24h: Number(use.get(t.id)?.cost_usd ?? 0),
    budget: budgetView(budget.get(t.id), now),
    access: accessState(t, now),
    running: live.get(t.id)?.in_flight ?? 0,
    waiting: live.get(t.id)?.queued ?? 0,
  }));
}

export type TenantStatusFilter = "all" | "active" | "suspended" | "archived";
export type TenantHas = "any" | "budget" | "hours" | "allowlist" | "guardrails" | "near-limit";
export type TenantSort = "requests" | "name" | "budget" | "newest";

export function filterTenants(rows: TenantRow[], f: { query: string; status: TenantStatusFilter; group: string; has: TenantHas }): TenantRow[] {
  const q = f.query.trim().toLowerCase();
  return rows.filter((r) => {
    const t = r.tenant;
    if (f.status !== "all" && t.status !== f.status) return false;
    if (f.group && t.fairshare_group !== f.group) return false;
    if (f.has === "budget" && !r.budget) return false;
    if (f.has === "hours" && !(t.weekly_windows?.length || t.active_from || t.active_until)) return false;
    if (f.has === "allowlist" && !t.allowed_models?.length) return false;
    if (f.has === "guardrails" && !t.guardrails_policy) return false;
    if (f.has === "near-limit" && !((r.budget?.share ?? 0) >= NEAR_BUDGET || r.waiting > 0)) return false;
    return !q || [t.name, t.organization, t.description, t.contact_email].join(" ").toLowerCase().includes(q);
  });
}

export function sortTenants(rows: TenantRow[], sort: TenantSort): TenantRow[] {
  const byName = (a: TenantRow, b: TenantRow) => a.tenant.name.localeCompare(b.tenant.name);
  const out = [...rows];
  if (sort === "name") return out.sort(byName);
  if (sort === "newest") return out.sort((a, b) => b.tenant.created_at.localeCompare(a.tenant.created_at) || byName(a, b));
  if (sort === "budget") return out.sort((a, b) => (b.budget?.share ?? -1) - (a.budget?.share ?? -1) || byName(a, b));
  return out.sort((a, b) => b.requests24h - a.requests24h || byName(a, b));
}

/** "200K tok/min · 8 in flight", or "No limits". */
export function limitsLabel(t: Pick<Tenant, "tokens_per_minute" | "max_in_flight">): string {
  const parts = [t.tokens_per_minute > 0 ? `${tokensShort(t.tokens_per_minute)} tok/min` : null, t.max_in_flight ? `${t.max_in_flight} in flight` : null].filter(Boolean);
  return parts.length ? parts.join(" · ") : "No limits";
}

/**
 * A tenant's share of its group's slots when every tenant in the group is
 * busy, at `weight` (for the weight editor's preview).
 */
export function groupShare(tenants: Pick<Tenant, "id" | "fairshare_group" | "weight" | "status">[], id: string, group: string, weight: number): number {
  const others = tenants.filter((t) => t.id !== id && t.fairshare_group === group && t.status === "active");
  const total = others.reduce((n, t) => n + t.weight, 0) + weight;
  return total > 0 ? weight / total : 1;
}

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

export interface KeyRow {
  key: ApiKey;
  tenantName: string;
  usage: KeyUsageSummary | undefined;
  budget: BudgetView | null;
  lastUsedMs: number;
}

export function buildKeyRows(keys: ApiKey[], tenants: Pick<Tenant, "id" | "name">[], usage: KeyUsageSummary[], budgets: BudgetUsage[], now = new Date()): KeyRow[] {
  const names = new Map(tenants.map((t) => [t.id, t.name]));
  const use = new Map(usage.map((u) => [u.key_id, u]));
  const budget = new Map(budgets.filter((b) => b.scope === "key").map((b) => [b.id, b]));
  return keys.map((k) => ({
    key: k,
    tenantName: names.get(k.tenant_id) ?? "Deleted tenant",
    usage: use.get(k.id),
    budget: budgetView(budget.get(k.id), now),
    lastUsedMs: use.get(k.id)?.last_used_ms ?? 0,
  }));
}

export interface KeyFilters {
  query: string;
  tenantId: string;
  status: "all" | "on" | "off";
  kind: "all" | "secret" | "identity";
  budget: "all" | "capped" | "near" | "none";
  unused: boolean;
}

export const EMPTY_KEY_FILTERS: KeyFilters = { query: "", tenantId: "", status: "all", kind: "all", budget: "all", unused: false };

const THIRTY_DAYS = 30 * 86_400_000;

export function filterKeys(rows: KeyRow[], f: KeyFilters, now = Date.now()): KeyRow[] {
  const q = f.query.trim().toLowerCase();
  return rows.filter((r) => {
    const k = r.key;
    if (f.tenantId && k.tenant_id !== f.tenantId) return false;
    if (f.status === "on" && k.disabled) return false;
    if (f.status === "off" && !k.disabled) return false;
    if (f.kind !== "all" && k.kind !== f.kind) return false;
    if (f.budget === "capped" && !r.budget) return false;
    if (f.budget === "near" && !((r.budget?.share ?? 0) >= NEAR_BUDGET)) return false;
    if (f.budget === "none" && r.budget) return false;
    if (f.unused && r.lastUsedMs > now - THIRTY_DAYS) return false;
    return !q || [k.name, k.key_prefix, k.description, r.tenantName, k.identity_subject ?? "", k.identity_issuer ?? ""].join(" ").toLowerCase().includes(q);
  });
}

export type KeySort = "last-used" | "requests" | "spend" | "name" | "budget" | "newest";

export function sortKeys(rows: KeyRow[], sort: KeySort): KeyRow[] {
  const byName = (a: KeyRow, b: KeyRow) => a.key.name.localeCompare(b.key.name);
  const out = [...rows];
  if (sort === "name") return out.sort(byName);
  if (sort === "newest") return out.sort((a, b) => b.key.created_at.localeCompare(a.key.created_at) || byName(a, b));
  if (sort === "requests") return out.sort((a, b) => (b.usage?.requests ?? 0) - (a.usage?.requests ?? 0) || byName(a, b));
  if (sort === "spend") return out.sort((a, b) => (b.usage?.cost_usd ?? 0) - (a.usage?.cost_usd ?? 0) || byName(a, b));
  if (sort === "budget") return out.sort((a, b) => (b.budget?.share ?? -1) - (a.budget?.share ?? -1) || byName(a, b));
  return out.sort((a, b) => b.lastUsedMs - a.lastUsedMs || Number(a.key.disabled) - Number(b.key.disabled) || byName(a, b));
}

/** "12 s ago", "4 min ago", "yesterday", "41 days ago", or "never". */
export function lastUsed(ms: number, now = Date.now()): string {
  if (!ms) return "never";
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 60) return `${s} s ago`;
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86_400) return `${Math.floor(s / 3600)} h ago`;
  const days = Math.floor(s / 86_400);
  return days === 1 ? "yesterday" : `${days} days ago`;
}

// ---------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------

export type Reach = "dashboard" | "dashboard-portal" | "portal" | "nothing" | "waiting";

/**
 * What a person can use. An admin gets the dashboard (and the portal too if
 * linked to a tenant); a user needs a tenant for the portal; a pending
 * account can use nothing until approved.
 */
export function reachOf(u: Pick<AdminUser, "role" | "status" | "tenantId">): Reach {
  if (u.status !== "active") return "waiting";
  if (u.role === "admin") return u.tenantId ? "dashboard-portal" : "dashboard";
  return u.tenantId ? "portal" : "nothing";
}

export function reachLabel(reach: Reach, tenantName?: string): string {
  switch (reach) {
    case "dashboard":
      return "Dashboard";
    case "dashboard-portal":
      return `Dashboard and portal${tenantName ? ` · ${tenantName}` : ""}`;
    case "portal":
      return `Portal${tenantName ? ` · ${tenantName}` : ""}`;
    case "nothing":
      return "Nothing yet: no tenant";
    default:
      return "Waiting for approval";
  }
}

// ---------------------------------------------------------------------------
// A tenant's page
// ---------------------------------------------------------------------------

/** A tenant's page address: by name, which is what people say. */
export function tenantHref(t: { name: string }, section?: string): string {
  return `/tenants/${encodeURIComponent(t.name)}${section ? `#${section}` : ""}`;
}

export interface TenantDay {
  day: string;
  requests: number;
  failed: number;
  cost: number;
  tokens: number;
}

export interface TenantOverviewData {
  now: number;
  requests24h: number;
  /** The day before, for the change; null when it could not be read. */
  previous24h: number | null;
  failed24h: number;
  /** The most common failure in the last day. */
  topFailure: { status_code: number; model: string; requests: number } | null;
  days: TenantDay[];
  models: { model: string; requests: number; tokens: number; cost: number }[];
  tokens30d: number;
  inputTokens30d: number;
  keys: KeyUsageSummary[];
  audit: { ts: string; actor: string; action: string; detail: unknown }[];
}

/** Every day from `start` to `end` (YYYY-MM-DD, inclusive), filled from the rows. */
export function fillDays(rows: Pick<TenantDay, "day" | "requests" | "failed" | "cost" | "tokens">[], start: string, end: string): TenantDay[] {
  const by = new Map(rows.map((r) => [r.day, r]));
  const out: TenantDay[] = [];
  const d = new Date(`${start}T00:00:00Z`);
  const last = new Date(`${end}T00:00:00Z`);
  while (d <= last) {
    const day = d.toISOString().slice(0, 10);
    const r = by.get(day);
    out.push({ day, requests: r?.requests ?? 0, failed: r?.failed ?? 0, cost: r?.cost ?? 0, tokens: r?.tokens ?? 0 });
    d.setUTCDate(d.getUTCDate() + 1);
  }
  return out;
}

/** "▲ 32% vs yesterday", "▼ 5.0% vs yesterday", "new since yesterday", or null. */
export function changeVs(now: number, before: number | null, word = "yesterday"): string | null {
  if (before === null) return null;
  if (before === 0) return now > 0 ? `new since ${word}` : null;
  const pct = ((now - before) / before) * 100;
  const size = Math.abs(pct) >= 10 ? Math.round(Math.abs(pct)).toString() : Math.abs(pct).toFixed(1);
  return `${pct >= 0 ? "▲" : "▼"} ${size}% vs ${word}`;
}
