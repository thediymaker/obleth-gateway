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
