// Canonical numeric/duration formatting for the dashboard. One implementation
// so every page agrees on casing and thresholds (1.2k / 3.4M / 1.1B, k at 1000).
import { formatNumber } from "@/lib/utils";

export function formatCompact(n: number): string {
  if (!Number.isFinite(n)) return "0";
  const sign = n < 0 ? "-" : "";
  const abs = Math.abs(n);
  if (abs < 1000) return `${sign}${abs < 10 ? String(Math.round(abs * 10) / 10) : String(Math.round(abs))}`;
  if (abs < 1_000_000) return `${sign}${(abs / 1000).toFixed(abs < 10_000 ? 1 : 0)}k`;
  if (abs < 1_000_000_000) return `${sign}${(abs / 1_000_000).toFixed(abs < 10_000_000 ? 1 : 0)}M`;
  return `${sign}${(abs / 1_000_000_000).toFixed(1)}B`;
}

export function formatDecimal(n: number): string {
  if (!Number.isFinite(n)) return "0";
  if (Math.abs(n) >= 10 || Number.isInteger(n)) return formatNumber(Math.round(n));
  return n.toFixed(1);
}

export function formatPct(n: number): string {
  if (!Number.isFinite(n)) return "0%";
  if (Math.abs(n) < 10 && n !== 0) return `${n.toFixed(1)}%`;
  return `${Math.round(n)}%`;
}

export function formatDelta(delta: number): string {
  const rounded = Math.abs(delta) < 0.05 ? 0 : delta;
  if (rounded === 0) return "0";
  const sign = rounded > 0 ? "+" : "-";
  const mag = Math.abs(rounded);
  return `${sign}${mag >= 10 || Number.isInteger(mag) ? formatNumber(Math.round(mag)) : mag.toFixed(1)}`;
}

/** Scheduler debt/share scores: fixed precision small, compact large. */
export function formatScore(n: number): string {
  if (!Number.isFinite(n)) return "0";
  if (Math.abs(n) >= 1000) return formatCompact(n);
  if (Math.abs(n) >= 10) return n.toFixed(0);
  return n.toFixed(2);
}

/** Sub-second durations stay in ms; longer ones read as seconds. */
export function formatDurationMs(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return "--";
  return ms < 1000 ? `${Math.round(ms)}ms` : `${(ms / 1000).toFixed(2)}s`;
}

/** Head…tail form for opaque IDs. Pair with a title attr carrying the full value. */
export function truncateId(id: string): string {
  return id.length > 12 ? `${id.slice(0, 8)}…${id.slice(-4)}` : id;
}

export function clamp(n: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, n));
}
