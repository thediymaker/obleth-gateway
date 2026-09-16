import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

export function formatNumber(n: number) {
  return new Intl.NumberFormat("en-US").format(n);
}

export function formatCurrency(n: number) {
  return new Intl.NumberFormat("en-US", {
    style: "currency",
    currency: "USD",
    minimumFractionDigits: 2,
    maximumFractionDigits: 6,
  }).format(n);
}

export async function getJson<T>(url: string): Promise<T> {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`fetch failed: ${url}`);
  return (await res.json()) as T;
}

// True if `tags` contains an entry whose base name (everything before an
// optional `:level` suffix, e.g. "vision" in "vision:2") matches `base`.
// Routing tags carry an optional strength suffix (`coding:3`); this lets any
// bare-tag check — a create/update derivation, a display filter — match a
// suffixed entry instead of silently missing it. Shared by the server model
// actions and the client components that filter/count models by tag.
export function tagsInclude(tags: string[] | null | undefined, base: string): boolean {
  return (tags ?? []).some((t) => t === base || t.startsWith(`${base}:`));
}

// Splits a stored tag entry into its base name and strength level, mirroring
// the gateway's `parse_tag_level`: base is everything before the first ':',
// the level is the digit after it, and a missing or out-of-range level clamps
// into 1..=3 rather than dropping the tag. `declared` distinguishes a tag the
// operator actually levelled from one that merely defaults to 1, so a display
// can label the first and stay quiet about the second.
//
// Shared rather than per-component: `coding:3` is internal ladder syntax and
// must never reach a reader, least of all in the developer portal.
export function parseTagLevel(raw: string): { base: string; level: number; declared: boolean } {
  const idx = raw.indexOf(":");
  if (idx === -1) return { base: raw, level: 1, declared: false };
  const base = raw.slice(0, idx);
  const level = Number(raw.slice(idx + 1));
  return {
    base,
    level: Number.isFinite(level) ? Math.min(3, Math.max(1, Math.trunc(level))) : 1,
    declared: true,
  };
}

// Human labels for the strength ladder; the numbers are an implementation
// detail an operator should not have to decode. 0 is the UI-only "Auto"
// sentinel: the tag is saved bare, and under hybrid tier sourcing the level
// derives from the model's cost rank instead of being pinned.
export const TAG_LEVEL_AUTO = 0;
export const TAG_LEVEL_LABELS: Record<number, string> = { 0: "Auto", 1: "Basic", 2: "Strong", 3: "Best" };
