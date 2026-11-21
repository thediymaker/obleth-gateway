// The dashboard's number, money, duration and percent formatting. Every page
// imports from here so one value reads the same everywhere: "1.2K" / "3.4M" /
// "1.1B" (capital K from 1,000), "850 ms" / "1.5 s", "$4.60" / "$1,235".
// Output is always en-US, never the browser's locale, so server and client
// renders match and figures read the same in every screenshot.

const INTEGER = new Intl.NumberFormat("en-US", { maximumFractionDigits: 0 });
const GROUPED = new Intl.NumberFormat("en-US", { maximumFractionDigits: 20 });
const ONE_DECIMAL = new Intl.NumberFormat("en-US", { maximumFractionDigits: 1 });

/** A count with thousands separators, as written: "12,345". Fractions are kept. */
export function formatNumber(n: number): string {
  return Number.isFinite(n) ? GROUPED.format(n) : "0";
}

/** Up to one decimal with separators: "1,234.5". For kWh, kg and the like. */
export function oneDecimal(n: number): string {
  return Number.isFinite(n) ? ONE_DECIMAL.format(n) : "0";
}

/** Large counts the short way: "950", "1.2K", "12K", "3.4M", "1.1B". */
export function compact(n: number): string {
  if (!Number.isFinite(n)) return "0";
  const abs = Math.abs(n);
  if (abs >= 1e9) return `${+(n / 1e9).toFixed(abs >= 1e10 ? 0 : 1)}B`;
  if (abs >= 1e6) return `${+(n / 1e6).toFixed(abs >= 1e7 ? 0 : 1)}M`;
  if (abs >= 1e3) return `${+(n / 1e3).toFixed(abs >= 1e4 ? 0 : 1)}K`;
  return String(Math.round(n));
}

/** A latency or short duration: "850 ms", "1.5 s", "12 s", "2.5 min"; "—" when unknown. */
export function formatMs(ms: number): string {
  if (!ms || !Number.isFinite(ms)) return "—";
  if (ms >= 60_000) return `${+(ms / 60_000).toFixed(1)} min`;
  if (ms >= 1000) return `${+(ms / 1000).toFixed(ms >= 10_000 ? 0 : 1)} s`;
  return `${Math.round(ms)} ms`;
}

/**
 * Dollars: "$0", "$4.60", "$1,235", "$12K". Below a cent it reads "< $0.01",
 * unless `precise` is set (per-request costs and prices), which keeps two
 * significant digits instead: "$0.0012", "$0.075".
 */
export function money(v: number, opts: { precise?: boolean } = {}): string {
  if (!Number.isFinite(v) || v === 0) return "$0";
  if (v < 0) return `-${money(-v, opts)}`;
  if (v >= 10_000) return `$${compact(v)}`;
  if (v >= 100) return `$${INTEGER.format(v)}`;
  if (v >= 1 || (v >= 0.01 && !opts.precise)) return `$${v.toFixed(2)}`;
  if (!opts.precise) return "< $0.01";
  const decimals = Math.max(2, 1 - Math.floor(Math.log10(v)));
  return `$${v.toFixed(decimals).replace(/(\.\d\d\d*?)0+$/, "$1")}`;
}

/** A per-token price as a price per million tokens: "$0.20", "$0.0015", or "free". */
export function perMillion(perToken: number): string {
  const v = perToken * 1_000_000;
  return v ? money(v, { precise: true }) : "free";
}

/** A share from 0 to 1 as a percent: "42%", one decimal under 10%: "3.1%". */
export function percent(share: number): string {
  if (!Number.isFinite(share)) return "0%";
  const p = share * 100;
  if (p !== 0 && Math.abs(p) < 10) return `${+p.toFixed(1)}%`;
  return `${Math.round(p)}%`;
}

// ---------------------------------------------------------------------------
// Time. Pages that render on the server pass `now` from the client where they
// already do, so the server and browser renders agree.
// ---------------------------------------------------------------------------

/** A moment as the APIs send it: epoch ms, an ISO string, or a Date. */
export type Moment = number | string | Date;

const toDate = (at: Moment) => (at instanceof Date ? at : new Date(at));
const valid = (d: Date) => !Number.isNaN(d.getTime());

/** Any date or time in en-US, for the rare format the helpers below don't cover. */
export function formatDate(at: Moment, options: Intl.DateTimeFormatOptions): string {
  const d = toDate(at);
  return valid(d) ? d.toLocaleString("en-US", options) : "—";
}

/** "2:20 PM". */
export function clock(at: Moment): string {
  return formatDate(at, { hour: "numeric", minute: "2-digit" });
}

/** "2:20:05 PM", for logs and anything that happens seconds apart. */
export function clockSeconds(at: Moment): string {
  return formatDate(at, { hour: "numeric", minute: "2-digit", second: "2-digit" });
}

/** "Nov 12", or "Nov 12, 2025" with `year`. `utc` reads a calendar day stored as UTC midnight. */
export function dateLabel(at: Moment, opts: { year?: boolean; utc?: boolean } = {}): string {
  return formatDate(at, { month: "short", day: "numeric", ...(opts.year ? { year: "numeric" } : {}), ...(opts.utc ? { timeZone: "UTC" } : {}) });
}

/** "Nov 12, 2:20 PM". */
export function dateTime(at: Moment): string {
  return formatDate(at, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
}

/** "Nov 12, 2025, 2:20:05 PM": the whole moment, for a title attribute. */
export function fullDateTime(at: Moment): string {
  return formatDate(at, { month: "short", day: "numeric", year: "numeric", hour: "numeric", minute: "2-digit", second: "2-digit" });
}

/** "2:20 PM" today, "Nov 12, 2:20 PM" on another day. */
export function clockOrDate(at: Moment, now: number): string {
  const d = toDate(at);
  return d.toDateString() === new Date(now).toDateString() ? clock(d) : dateTime(d);
}

const DAY_S = 86_400;

/**
 * How long ago, in words: "just now", "40 s ago", "4 min ago", "3 h ago",
 * "2 d ago", and the date past 60 days. `short` is for narrow columns: "now",
 * "40s", "4m", "3h", "2d". No time reads "never" ("" when short); a time in
 * the future reads "just now", since clocks drift a little.
 */
export function ago(at: Moment | null | undefined, now: number, opts: { short?: boolean } = {}): string {
  if (at === null || at === undefined || at === "" || at === 0) return opts.short ? "" : "never";
  const d = toDate(at);
  if (!valid(d)) return opts.short ? "" : "never";
  const s = Math.max(0, Math.floor((now - d.getTime()) / 1000));
  const short = opts.short;
  if (s < (short ? 60 : 5)) return short ? "now" : "just now";
  if (s < 60) return `${s} s ago`;
  if (s < 3600) return short ? `${Math.floor(s / 60)}m` : `${Math.floor(s / 60)} min ago`;
  if (s < DAY_S) return short ? `${Math.floor(s / 3600)}h` : `${Math.floor(s / 3600)} h ago`;
  if (s < 60 * DAY_S) return short ? `${Math.floor(s / DAY_S)}d` : `${Math.floor(s / DAY_S)} d ago`;
  return dateLabel(d, { year: d.getFullYear() !== new Date(now).getFullYear() });
}

/** How long ago a count of seconds back from now was: the APIs that report
 * "last seen N s ago" as a number. No number reads "never". */
export function secsAgo(secs: number | null | undefined, now = Date.now()): string {
  return secs === null || secs === undefined ? "never" : ago(now - secs * 1000, now);
}

/** A request's time in a log: "12 s ago", "4 min ago", then the clock, or the date and clock on another day. */
export function whenLogged(at: Moment, now: number): string {
  const s = Math.floor((now - toDate(at).getTime()) / 1000);
  return s < 3600 ? ago(at, now) : clockOrDate(at, now);
}

/** A length of time in words: "45 s", "6 min 24 s", "12 min", "3 h 5 min", "2 d 4 h". */
export function duration(ms: number): string {
  const s = Math.max(0, Math.round((Number.isFinite(ms) ? ms : 0) / 1000));
  if (s < 60) return `${s} s`;
  if (s < 600) return s % 60 ? `${Math.floor(s / 60)} min ${s % 60} s` : `${s / 60} min`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m} min`;
  const h = Math.floor(m / 60);
  if (h < 24) return m % 60 ? `${h} h ${m % 60} min` : `${h} h`;
  const d = Math.floor(h / 24);
  return h % 24 ? `${d} d ${h % 24} h` : `${d} d`;
}

export function clamp(n: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, n));
}

const BYTE_UNITS = ["B", "KB", "MB", "GB", "TB"];

/** Byte counts scaled to the largest unit that keeps the value >= 1. Whole
 * bytes render without a decimal; everything above KB keeps one. */
export function formatBytes(n: number): string {
  if (!Number.isFinite(n)) return "0 B";
  if (n < 1024) return `${Math.max(0, Math.round(n))} B`;
  let value = n;
  let unit = 0;
  while (value >= 1024 && unit < BYTE_UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(1)} ${BYTE_UNITS[unit]}`;
}
