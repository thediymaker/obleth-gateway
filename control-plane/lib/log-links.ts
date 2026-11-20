/**
 * A link to the Request logs page with filters already chosen: the way every
 * other page says "show me these requests". `since` and `until` are epoch
 * millis or `YYYY-MM-DD` days. A day is a UTC day, as the daily rollup that
 * Reports and the team pages count from cuts them, and a day's `until` means
 * the end of that day.
 */
export interface LogsLink {
  status?: "error" | "success";
  /** One exact HTTP status. */
  code?: number;
  /** Only the platform's failures, or only the callers'. */
  fault?: "platform" | "client";
  /** One failure class (lib/failure-classes.ts), or several joined by commas. */
  class?: string;
  model?: string;
  team?: string;
  key?: string;
  session?: string;
  window?: "15m" | "1h" | "24h" | "7d" | "30d";
  since?: number | string;
  until?: number | string;
  /** One request, opened: the page looks back 30 days for it. */
  requestId?: string;
}

export function logsHref(link: LogsLink): string {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(link)) if (v !== undefined && v !== "") p.set(k, String(v));
  const q = p.toString();
  return q ? `/logs?${q}` : "/logs";
}

/** A `since`/`until` value as epoch millis: a day starts at 00:00 UTC, and a day's end is the next 00:00 UTC less a millisecond. */
export function linkTime(value: string | undefined, end: boolean): number | undefined {
  if (!value) return undefined;
  if (/^\d+$/.test(value)) return Number(value);
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
  if (!m) return undefined;
  const start = Date.UTC(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
  return end ? start + 86_400_000 - 1 : start;
}
