/**
 * A link to the Request logs page with filters already chosen: the way every
 * other page says "show me these requests". `since` and `until` are epoch
 * millis or `YYYY-MM-DD` days (a day's `until` means the end of that day, in
 * the reader's time).
 */
export interface LogsLink {
  status?: "error" | "success";
  /** One exact HTTP status. */
  code?: number;
  model?: string;
  team?: string;
  key?: string;
  session?: string;
  window?: "15m" | "1h" | "24h" | "7d" | "30d";
  since?: number | string;
  until?: number | string;
}

export function logsHref(link: LogsLink): string {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(link)) if (v !== undefined && v !== "") p.set(k, String(v));
  const q = p.toString();
  return q ? `/logs?${q}` : "/logs";
}

/** A `since`/`until` value as epoch millis: a day starts at its local midnight, and a day's end is the next midnight less a millisecond. */
export function linkTime(value: string | undefined, end: boolean): number | undefined {
  if (!value) return undefined;
  if (/^\d+$/.test(value)) return Number(value);
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
  if (!m) return undefined;
  const start = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
  if (!end) return start.getTime();
  start.setDate(start.getDate() + 1);
  return start.getTime() - 1;
}
