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
