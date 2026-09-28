import type { UsageDailyGroupBy, UsageDailyRow } from "@/lib/obleth";
import type { ExportColumn } from "@/lib/usage-export";

/**
 * The Reports page's pure logic: date ranges and the period before, totals,
 * the chart's per-day values (optionally split by team or model), and the
 * export presets.
 */

const DAY_MS = 86_400_000;

export type RangePreset = "7d" | "30d" | "month" | "last-month" | "custom";

export const RANGE_PRESETS: { value: RangePreset; label: string }[] = [
  { value: "7d", label: "7 days" },
  { value: "30d", label: "30 days" },
  { value: "month", label: "This month" },
  { value: "last-month", label: "Last month" },
  { value: "custom", label: "Custom…" },
];

export interface DayRange {
  start: string;
  end: string;
}

/** `YYYY-MM-DD` in local time: a day is the day the person reading means. */
export function isoDay(d: Date): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

function parseDay(s: string): Date {
  const [y, m, d] = s.split("-").map(Number);
  return new Date(y, m - 1, d);
}

function addDays(s: string, n: number): string {
  const d = parseDay(s);
  d.setDate(d.getDate() + n);
  return isoDay(d);
}

export function daysBetween(r: DayRange): number {
  return Math.round((parseDay(r.end).getTime() - parseDay(r.start).getTime()) / DAY_MS) + 1;
}

export function presetRange(preset: Exclude<RangePreset, "custom">, today = new Date()): DayRange {
  const t = isoDay(today);
  if (preset === "7d") return { start: addDays(t, -6), end: t };
  if (preset === "30d") return { start: addDays(t, -29), end: t };
  if (preset === "month") return { start: isoDay(new Date(today.getFullYear(), today.getMonth(), 1)), end: t };
  const first = new Date(today.getFullYear(), today.getMonth() - 1, 1);
  const last = new Date(today.getFullYear(), today.getMonth(), 0);
  return { start: isoDay(first), end: isoDay(last) };
}

/**
 * The period a range is compared with. A calendar month compares with the
 * same days of the month before (Sep 1–27 against Aug 1–27, all of August
 * against all of July); any other range with the same number of days just
 * before it.
 */
export function previousRange(r: DayRange, preset: RangePreset): DayRange {
  if (preset === "month" || preset === "last-month") {
    const s = parseDay(r.start);
    const e = parseDay(r.end);
    const ps = new Date(s.getFullYear(), s.getMonth() - 1, 1);
    const lastOfPrev = new Date(s.getFullYear(), s.getMonth(), 0).getDate();
    const wholeMonth = preset === "last-month";
    const pe = new Date(ps.getFullYear(), ps.getMonth(), wholeMonth ? lastOfPrev : Math.min(e.getDate(), lastOfPrev));
    return { start: isoDay(ps), end: isoDay(pe) };
  }
  const n = daysBetween(r);
  return { start: addDays(r.start, -n), end: addDays(r.start, -1) };
}

export function rangeLabel(r: DayRange): string {
  const fmt = (s: string, year: boolean) => parseDay(s).toLocaleDateString([], { month: "short", day: "numeric", ...(year ? { year: "numeric" } : {}) });
  const sameYear = r.start.slice(0, 4) === r.end.slice(0, 4);
  return r.start === r.end ? fmt(r.start, true) : `${fmt(r.start, !sameYear)} – ${fmt(r.end, true)}`;
}

/** Every day in the range, oldest first. */
export function eachDay(r: DayRange): string[] {
  const out: string[] = [];
  for (let d = r.start; d <= r.end && out.length < 400; d = addDays(d, 1)) out.push(d);
  return out;
}

// ---------------------------------------------------------------------------
// Totals
// ---------------------------------------------------------------------------

export interface Totals {
  requests: number;
  failed: number;
  succeeded: number;
  inputTokens: number;
  outputTokens: number;
  tokens: number;
  cost: number;
  cacheHits: number;
  cacheMisses: number;
  energyWh: number;
  energyCost: number;
  co2G: number;
  /** Mean first token over succeeded requests (the rollup keeps per-row averages). */
  avgTtftMs: number;
  avgTotalMs: number;
  estimatedTokens: number;
}

export function totals(rows: UsageDailyRow[]): Totals {
  const t: Totals = { requests: 0, failed: 0, succeeded: 0, inputTokens: 0, outputTokens: 0, tokens: 0, cost: 0, cacheHits: 0, cacheMisses: 0, energyWh: 0, energyCost: 0, co2G: 0, avgTtftMs: 0, avgTotalMs: 0, estimatedTokens: 0 };
  let ttft = 0;
  let total = 0;
  for (const r of rows) {
    t.requests += r.requests;
    t.failed += r.error_requests;
    t.succeeded += r.success_requests;
    t.inputTokens += r.input_tokens;
    t.outputTokens += r.output_tokens;
    t.tokens += r.total_tokens;
    t.cost += r.cost_usd;
    t.cacheHits += r.cache_hits;
    t.cacheMisses += r.cache_misses;
    t.energyWh += r.energy_wh;
    t.energyCost += r.energy_cost_usd;
    t.co2G += r.co2_g;
    t.estimatedTokens += r.estimated_tokens;
    ttft += r.avg_ttft_ms * r.success_requests;
    total += r.avg_total_ms * r.success_requests;
  }
  t.avgTtftMs = t.succeeded ? ttft / t.succeeded : 0;
  t.avgTotalMs = t.succeeded ? total / t.succeeded : 0;
  return t;
}

/** "▲ 18% vs the previous period", or null when there is nothing to compare with. */
export function change(current: number, previous: number | null | undefined, against: string): string | null {
  if (previous == null || !Number.isFinite(previous)) return null;
  if (previous === 0) return current === 0 ? `no change vs ${against}` : `new since ${against}`;
  const pct = ((current - previous) / previous) * 100;
  if (Math.abs(pct) < 0.5) return `no change vs ${against}`;
  return `${pct > 0 ? "▲" : "▼"} ${Math.abs(pct) >= 10 ? Math.round(Math.abs(pct)) : Math.abs(pct).toFixed(1)}% vs ${against}`;
}

// ---------------------------------------------------------------------------
// The chart
// ---------------------------------------------------------------------------

export type Measure = "requests" | "tokens" | "spend" | "ttft";

export const MEASURES: { value: Measure; label: string }[] = [
  { value: "requests", label: "Requests" },
  { value: "tokens", label: "Tokens" },
  { value: "spend", label: "Spend" },
  { value: "ttft", label: "First token" },
];

export type Split = "none" | "tenant" | "model";

/** A measure over a set of rollup rows. First token is averaged, the others summed. */
export function measureOf(rows: UsageDailyRow[], m: Measure): number {
  const t = totals(rows);
  return m === "requests" ? t.requests : m === "tokens" ? t.tokens : m === "spend" ? t.cost : t.avgTtftMs;
}

/** Grey steps, lightest for the largest share; "everyone else" darkest. */
export const SPLIT_TONES = ["hsl(240 5% 88%)", "hsl(240 4% 66%)", "hsl(240 4% 50%)", "hsl(240 4% 38%)"];
export const REST_TONE = "hsl(240 4% 24%)";
export const SPLIT_TOP = 4;

export interface ChartDay {
  day: string;
  value: number;
  previous: number | null;
  /** With a split: each series' value that day, keyed by series id. */
  parts: Record<string, number>;
}

export interface ChartSeries { id: string; label: string; color: string }

/**
 * Per-day values for the chart. Unsplit, each day carries the same day of the
 * previous period (by position, so a 7-day range lines up with the 7 days
 * before). Split, the four largest over the whole range get a series each and
 * the rest sum into one; first token can't be split into parts that add up,
 * so it is never split.
 */
export function buildChart(
  range: DayRange,
  measure: Measure,
  split: Split,
  days: UsageDailyRow[],
  previous: { range: DayRange; rows: UsageDailyRow[] } | null,
  splitRows: UsageDailyRow[],
  labelFor: (row: UsageDailyRow) => string,
): { days: ChartDay[]; series: ChartSeries[] } {
  const all = eachDay(range);
  const byDay = new Map<string, UsageDailyRow[]>();
  for (const r of days) byDay.set(r.day, [...(byDay.get(r.day) ?? []), r]);
  const prevDays = previous ? eachDay(previous.range) : [];
  const prevMap = new Map((previous?.rows ?? []).map((r) => [r.day, r]));
  const splitting = split !== "none" && measure !== "ttft";

  const keyOf = (r: UsageDailyRow) => (split === "tenant" ? r.tenant_id : r.model);
  const seriesTotals = new Map<string, { label: string; value: number }>();
  if (splitting) {
    for (const r of splitRows) {
      const k = keyOf(r);
      const cur = seriesTotals.get(k) ?? { label: labelFor(r), value: 0 };
      cur.value += measureOf([r], measure);
      seriesTotals.set(k, cur);
    }
  }
  const ranked = [...seriesTotals.entries()].sort((a, b) => b[1].value - a[1].value);
  const top = ranked.slice(0, SPLIT_TOP).map(([id, v], i) => ({ id, label: v.label, color: SPLIT_TONES[i] }));
  const series: ChartSeries[] = splitting
    ? [...top, ...(ranked.length > SPLIT_TOP ? [{ id: "__rest", label: `${ranked.length - SPLIT_TOP} more`, color: REST_TONE }] : [])]
    : [];
  const topIds = new Set(top.map((s) => s.id));
  const splitByDay = new Map<string, UsageDailyRow[]>();
  for (const r of splitRows) splitByDay.set(r.day, [...(splitByDay.get(r.day) ?? []), r]);

  return {
    series,
    days: all.map((day, i) => {
      const rows = byDay.get(day) ?? [];
      const parts: Record<string, number> = {};
      if (splitting) {
        for (const r of splitByDay.get(day) ?? []) {
          const id = topIds.has(keyOf(r)) ? keyOf(r) : "__rest";
          parts[id] = (parts[id] ?? 0) + measureOf([r], measure);
        }
      }
      const prevRow = prevMap.get(prevDays[i] ?? "");
      return {
        day,
        value: measureOf(rows, measure),
        previous: previous && i < prevDays.length ? (prevRow ? measureOf([prevRow], measure) : 0) : null,
        parts,
      };
    }),
  };
}

// ---------------------------------------------------------------------------
// The breakdown table
// ---------------------------------------------------------------------------

export type TableGroup = "day" | "tenant" | "key" | "model" | "key_model";

export const TABLE_GROUPS: { value: TableGroup; label: string; plural: string }[] = [
  { value: "day", label: "Day", plural: "days" },
  { value: "tenant", label: "Team", plural: "teams" },
  { value: "key", label: "Key", plural: "keys" },
  { value: "model", label: "Model", plural: "models" },
  { value: "key_model", label: "Key + model", plural: "rows" },
];

export type SortColumn = "label" | "requests" | "error_requests" | "input_tokens" | "output_tokens" | "cost_usd" | "avg_ttft_ms" | "energy_wh";

export function sortRows<T extends UsageDailyRow & { label: string }>(rows: T[], col: SortColumn, desc: boolean): T[] {
  const dir = desc ? -1 : 1;
  return [...rows].sort((a, b) => {
    const v = col === "label" ? a.label.localeCompare(b.label) : (a[col] as number) - (b[col] as number);
    return v * dir || a.label.localeCompare(b.label);
  });
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

export type ExportPresetId = "team" | "key_model" | "daily" | "everything" | "custom";

export interface ExportPreset {
  id: ExportPresetId;
  title: string;
  description: string;
  group: UsageDailyGroupBy;
  columns: ExportColumn[];
}

export const EXPORT_PRESETS: ExportPreset[] = [
  {
    id: "team",
    title: "By team",
    description: "One row per team for the whole range: who used how much.",
    group: "tenant",
    columns: ["start_day", "end_day", "tenant_name", "requests", "error_requests", "input_tokens", "output_tokens", "total_tokens", "cost_usd", "energy_kwh"],
  },
  {
    id: "key_model",
    title: "By key and model",
    description: "Which key used which model, across the range.",
    group: "key_model",
    columns: ["start_day", "end_day", "tenant_name", "key_name", "key_prefix", "model", "requests", "error_requests", "input_tokens", "output_tokens", "cost_usd"],
  },
  {
    id: "daily",
    title: "Daily totals",
    description: "One row a day, for charting in a spreadsheet.",
    group: "day",
    columns: ["day", "requests", "success_requests", "error_requests", "total_tokens", "cache_hits", "avg_ttft_ms", "cost_usd", "energy_kwh", "co2_g"],
  },
  {
    id: "everything",
    title: "Every day, key and model",
    description: "The most detail the rollup keeps: each day for each key and model.",
    group: "day_key_model",
    columns: ["day", "tenant_name", "key_name", "key_prefix", "model", "requests", "success_requests", "error_requests", "input_tokens", "output_tokens", "total_tokens", "estimated_tokens", "cache_hits", "cache_misses", "avg_ttft_ms", "avg_total_ms", "cost_usd", "energy_kwh", "co2_g", "energy_cost_usd"],
  },
];

export const EXPORT_GROUPS: { value: UsageDailyGroupBy; label: string }[] = [
  { value: "tenant", label: "Per team" },
  { value: "key", label: "Per key" },
  { value: "model", label: "Per model" },
  { value: "key_model", label: "Per key and model" },
  { value: "day", label: "Per day" },
  { value: "day_tenant", label: "Per day and team" },
  { value: "day_model", label: "Per day and model" },
  { value: "day_key_model", label: "Per day, key and model" },
];

export const COLUMN_GROUPS: { name: string; columns: ExportColumn[] }[] = [
  { name: "Period", columns: ["day", "start_day", "end_day"] },
  { name: "Who", columns: ["tenant_name", "tenant_id", "key_name", "key_prefix", "key_id"] },
  { name: "What", columns: ["model"] },
  { name: "Volume", columns: ["requests", "success_requests", "error_requests", "input_tokens", "output_tokens", "total_tokens", "estimated_tokens"] },
  { name: "Speed and cache", columns: ["avg_ttft_ms", "avg_total_ms", "cache_hits", "cache_misses"] },
  { name: "Cost and energy", columns: ["cost_usd", "energy_kwh", "co2_g", "energy_cost_usd"] },
];

const COLUMNS_KEY = "obleth:reports:export-columns";

export function loadExportColumns(): ExportColumn[] | null {
  try {
    const v = JSON.parse(localStorage.getItem(COLUMNS_KEY) ?? "null");
    return Array.isArray(v) && v.every((c) => typeof c === "string") ? (v as ExportColumn[]) : null;
  } catch {
    return null;
  }
}

export function storeExportColumns(cols: ExportColumn[] | null) {
  try {
    if (cols) localStorage.setItem(COLUMNS_KEY, JSON.stringify(cols));
    else localStorage.removeItem(COLUMNS_KEY);
  } catch {
    // Storage off or full: the choice just isn't remembered.
  }
}
