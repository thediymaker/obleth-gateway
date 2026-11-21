import type { ModelSpeedPoint } from "@/lib/obleth";
import { daysBetween, eachDay, type DayRange } from "@/lib/reports-model";
import { linkTime } from "@/lib/log-links";
import { dateLabel, formatDate } from "@/lib/format";

/**
 * The Reports page's model speed section: one row per model with its speed
 * against the period before, a square per hour or day shaded by how that
 * hour or day compared with the model's own usual speed, and the paths of
 * the chart an opened model shows.
 */

/** Under this many measured requests a median says little, so the row is hidden behind "few". */
export const FEW_REQUESTS = 20;
/** A model this much slower than the period before counts as slowed down. */
export const SLOWDOWN = 0.2;
/** An hour or day at or under this share of the model's usual speed counts as slow. */
export const SLOW_DAY = 0.8;

export type SpeedSort = "busiest" | "fastest" | "slowest" | "slowed";

export const SPEED_SORTS: { value: SpeedSort; label: string }[] = [
  { value: "busiest", label: "Busiest" },
  { value: "fastest", label: "Fastest" },
  { value: "slowest", label: "Slowest" },
  { value: "slowed", label: "Slowed down" },
];

export interface SpeedCell {
  t: number;
  tps: number;
  slowTps: number;
  rated: number;
  /** This hour or day's median against the model's median over the whole range. */
  ratio: number;
}

export interface SpeedRow {
  model: string;
  requests: number;
  rated: number;
  tps: number;
  slowTps: number;
  promptTps: number;
  ttftMs: number;
  previousTps: number | null;
  /** Change in speed against the period before, as a fraction (-0.25 is 25% slower). */
  change: number | null;
  slowedDown: boolean;
  /** Too few measured requests for the median to mean much. */
  few: boolean;
  /** One per hour or day of the range, null where nothing was measured. */
  cells: (SpeedCell | null)[];
}

/**
 * One row per model that produced tokens in the range. Models with no measured
 * requests (embeddings, images, audio in, every request failed) are left out.
 * A change is only given when both periods have something to compare.
 */
export function speedRows(current: ModelSpeedPoint[], previous: ModelSpeedPoint[] | null, series: ModelSpeedPoint[], starts: number[]): SpeedRow[] {
  const before = new Map((previous ?? []).filter((p) => p.rated > 0).map((p) => [p.model, p]));
  const at = new Map<string, ModelSpeedPoint>();
  for (const p of series) if (p.rated > 0) at.set(`${p.bucket_ms}|${p.model}`, p);
  return current
    .filter((p) => p.rated > 0)
    .map((p) => {
      const prev = before.get(p.model);
      const change = prev && prev.gen_tokens_per_sec > 0 ? (p.gen_tokens_per_sec - prev.gen_tokens_per_sec) / prev.gen_tokens_per_sec : null;
      const few = p.rated < FEW_REQUESTS;
      return {
        model: p.model,
        requests: p.requests,
        rated: p.rated,
        tps: p.gen_tokens_per_sec,
        slowTps: p.slow_tokens_per_sec,
        promptTps: p.prompt_tokens_per_sec,
        ttftMs: p.p50_ttft_ms,
        previousTps: prev ? prev.gen_tokens_per_sec : null,
        change,
        slowedDown: change !== null && change <= -SLOWDOWN && !few && (prev?.rated ?? 0) >= FEW_REQUESTS,
        few,
        cells: starts.map((t) => {
          const b = at.get(`${t}|${p.model}`);
          return b ? { t, tps: b.gen_tokens_per_sec, slowTps: b.slow_tokens_per_sec, rated: b.rated, ratio: p.gen_tokens_per_sec > 0 ? b.gen_tokens_per_sec / p.gen_tokens_per_sec : 1 } : null;
        }),
      };
    });
}

export function sortSpeedRows(rows: SpeedRow[], sort: SpeedSort): SpeedRow[] {
  const by = (a: SpeedRow, b: SpeedRow) =>
    sort === "fastest" ? b.tps - a.tps
    : sort === "slowest" ? a.tps - b.tps
    : sort === "slowed" ? (a.change ?? Infinity) - (b.change ?? Infinity)
    : b.rated - a.rated;
  return [...rows].sort((a, b) => by(a, b) || b.rated - a.rated || a.model.localeCompare(b.model));
}

export type SpeedBucket = "hour" | "day";

/** Hours for a range of a day or two, days for anything longer. */
export function speedBucket(range: DayRange): SpeedBucket {
  return daysBetween(range) <= 2 ? "hour" : "day";
}

/** The start of every UTC hour or day in the range, as epoch millis. */
export function bucketStarts(range: DayRange, bucket: SpeedBucket, now = Date.now()): number[] {
  const days = eachDay(range).map((d) => linkTime(d, false)!);
  if (bucket === "day") return days;
  const out: number[] = [];
  const end = Math.min(linkTime(range.end, true)!, now);
  for (const d = new Date(days[0]); d.getTime() <= end && out.length < 24 * 3; d.setUTCHours(d.getUTCHours() + 1)) out.push(d.getTime());
  return out;
}

/**
 * A square's shade: dim at the model's usual speed, brightening as it slows,
 * brightest at half its usual speed or worse.
 */
export function shade(ratio: number): string {
  const slow = Math.max(0, Math.min(1, (0.97 - ratio) / 0.47));
  return `hsl(240 5% ${Math.round(17 + slow * 71)}%)`;
}

/** The shades the legend shows, usual first. */
export const SHADE_STEPS = [1, 0.85, 0.73, 0.62, 0.5].map(shade);

export function bucketLabel(t: number, bucket: SpeedBucket): string {
  const d = new Date(t);
  return bucket === "day"
    ? dateLabel(d, { utc: true })
    : `${formatDate(d, { month: "short", day: "numeric", hour: "numeric", timeZone: "UTC" })} UTC`;
}

/** The header's label over each square: a full date where a month or day starts, else just the day or hour. */
export function columnLabels(starts: number[], bucket: SpeedBucket): string[] {
  const every = Math.max(1, Math.ceil(starts.length / 16));
  return starts.map((t, i) => {
    if (i % every) return "";
    const d = new Date(t);
    const date = () => dateLabel(d, { utc: true });
    if (bucket === "day") return i === 0 || d.getUTCDate() === 1 ? date() : String(d.getUTCDate());
    return i === 0 || d.getUTCHours() === 0 ? date() : formatDate(d, { hour: "numeric", timeZone: "UTC" });
  });
}

function listOf(items: string[]): string {
  return items.length <= 1 ? items.join("") : `${items.slice(0, -1).join(", ")} and ${items[items.length - 1]}`;
}

/** One plain sentence or two about how the model's speed went over the range. */
export function describeSpeed(row: SpeedRow, bucket: SpeedBucket): string {
  const unit = bucket === "hour" ? "hour" : "day";
  const measured = row.cells.filter((c): c is SpeedCell => c !== null);
  if (!measured.length) return "";
  const parts: string[] = [];
  const slow = measured.filter((c) => c.ratio <= SLOW_DAY);
  const worst = measured.reduce((w, c) => (c.ratio < w.ratio ? c : w));
  if (!slow.length) {
    const spread = Math.round((1 - worst.ratio) * 100);
    parts.push(spread <= 0 ? `At or above its usual speed every ${unit} it was used.` : `Within ${spread}% of its usual speed every ${unit} it was used.`);
  } else {
    const low = `${Math.round(worst.ratio * 100)}% of usual`;
    parts.push(slow.length <= 3
      ? `Slower than usual on ${listOf(slow.map((c) => bucketLabel(c.t, bucket)))}, as low as ${low}.`
      : `Slower than usual on ${slow.length} ${unit}s, as low as ${low} on ${bucketLabel(worst.t, bucket)}.`);
    const last = measured[measured.length - 1];
    parts.push(last.ratio <= SLOW_DAY ? `Still slow on ${bucketLabel(last.t, bucket)}.` : `Back to usual by ${bucketLabel(last.t, bucket)}.`);
  }
  const empty = row.cells.length - measured.length;
  if (empty > 0) parts.push(`No requests on ${empty} ${unit}${empty === 1 ? "" : "s"}.`);
  return parts.join(" ");
}

/** A round top for the chart's scale. */
export function niceTop(v: number): number {
  if (v >= 20) return Math.ceil(v / 10) * 10;
  if (v >= 2) return Math.ceil(v);
  return Math.ceil(v * 10) / 10 || 1;
}

/**
 * SVG paths for an opened model's chart, drawn in a `width` × `height` box
 * whose columns line up with the squares above it: the median, a band down to
 * the slowest tenth, and the usual speed. Hours or days with no requests stay
 * gaps; a lone measured one is a short flat stroke.
 */
export function detailPaths(cells: (SpeedCell | null)[], usual: number, width: number, height: number) {
  const step = width / Math.max(cells.length, 1);
  const top = niceTop(Math.max(usual, ...cells.map((c) => c?.tps ?? 0)) * 1.15);
  const y = (v: number) => (height - (v / top) * height).toFixed(1);
  const x = (i: number) => ((i + 0.5) * step).toFixed(1);
  let median = "";
  let band = "";
  let run: number[] = [];
  const flush = () => {
    if (run.length === 1) {
      const c = cells[run[0]]!;
      const a = ((run[0] + 0.5) * step - step * 0.3).toFixed(1);
      const b = ((run[0] + 0.5) * step + step * 0.3).toFixed(1);
      median += `M${a} ${y(c.tps)} L${b} ${y(c.tps)} `;
      band += `M${a} ${y(c.tps)} L${b} ${y(c.tps)} L${b} ${y(c.slowTps)} L${a} ${y(c.slowTps)} Z `;
    } else if (run.length > 1) {
      median += run.map((i, k) => `${k ? "L" : "M"}${x(i)} ${y(cells[i]!.tps)}`).join(" ") + " ";
      band += run.map((i, k) => `${k ? "L" : "M"}${x(i)} ${y(cells[i]!.tps)}`).join(" ") + " " + [...run].reverse().map((i) => `L${x(i)} ${y(cells[i]!.slowTps)}`).join(" ") + " Z ";
    }
    run = [];
  };
  cells.forEach((c, i) => (c ? run.push(i) : flush()));
  flush();
  return { median: median.trim(), band: band.trim(), usual: `M0 ${y(usual)} H${width}`, top };
}

export function tps(v: number): string {
  if (!v || !Number.isFinite(v)) return "—";
  return v >= 100 ? `${Math.round(v)}` : v.toFixed(1);
}

/** "▼ 32%" style label for a change, or null. */
export function changeLabel(change: number | null): string | null {
  if (change === null || !Number.isFinite(change)) return null;
  const pct = Math.abs(change * 100);
  if (pct < 0.5) return "same";
  return `${change > 0 ? "▲" : "▼"} ${pct >= 10 ? Math.round(pct) : pct.toFixed(1)}%`;
}
