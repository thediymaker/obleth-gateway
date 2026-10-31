import { logsHref } from "@/lib/log-links";
import { NIL_REQUEST_ID } from "@/lib/logs-model";
import type { UsageFailureGroup, UsageFailureGroups } from "@/lib/obleth";

/**
 * The Failures page's pure logic: its ranges, the headline sentence, how
 * long ago a group was seen, and the request-log link behind each group.
 */

export type FailureRange = "1h" | "24h" | "7d";

export const FAILURE_RANGES: { id: FailureRange; label: string; ms: number; words: string }[] = [
  { id: "1h", label: "1h", ms: 3_600_000, words: "the last hour" },
  { id: "24h", label: "24h", ms: 86_400_000, words: "the last 24 hours" },
  { id: "7d", label: "7d", ms: 7 * 86_400_000, words: "the last 7 days" },
];

export function isFailureRange(value: unknown): value is FailureRange {
  return FAILURE_RANGES.some((r) => r.id === value);
}

export function rangeOf(id: FailureRange) {
  return FAILURE_RANGES.find((r) => r.id === id) ?? FAILURE_RANGES[1];
}

/** Which groups the table shows: every group, or one side's. */
export type FailureSide = "all" | "platform" | "client";

/**
 * "412 failed of 661,802 requests: 87 on the platform's side, 325 from
 * callers", or what the window holds when nothing failed.
 */
export function failureHeadline(data: Pick<UsageFailureGroups, "requests" | "failures" | "platform_failures">, range: FailureRange): string {
  const n = (v: number) => v.toLocaleString();
  if (!data.requests) return `No requests in ${rangeOf(range).words}`;
  if (!data.failures) return `None of ${n(data.requests)} ${data.requests === 1 ? "request" : "requests"} failed`;
  const callers = Math.max(0, data.failures - data.platform_failures);
  return `${n(data.failures)} failed of ${n(data.requests)} ${data.requests === 1 ? "request" : "requests"}: ${n(data.platform_failures)} on the platform's side, ${n(callers)} from callers`;
}

/** The failures as a share of every request, "0.013%"; empty when there were none. */
export function share(part: number, whole: number): string {
  if (!whole || !part) return "";
  const pct = (part / whole) * 100;
  return `${pct >= 10 ? Math.round(pct) : pct >= 1 ? pct.toFixed(1) : pct >= 0.01 ? pct.toFixed(2) : "< 0.01"}%`;
}

/** "12 s ago", "4 min ago", "3 h ago", "2 days ago". */
export function seenAgo(ms: number, now = Date.now()): string {
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 60) return `${s} s ago`;
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86_400 * 2) return `${Math.floor(s / 3600)} h ago`;
  return `${Math.floor(s / 86_400)} days ago`;
}

/** A failure without a key (refused before one was read) carries the nil id. */
export const hasKey = (g: Pick<UsageFailureGroup, "key_id">) => !!g.key_id && g.key_id !== NIL_REQUEST_ID;

/** The request log narrowed to one group: its key (or team), model, status and class, over the page's range. */
export function groupLogsHref(g: UsageFailureGroup, range: FailureRange): string {
  return logsHref({
    code: g.status_code,
    class: g.error_class || undefined,
    model: g.model || undefined,
    key: hasKey(g) ? g.key_id : undefined,
    team: !hasKey(g) && g.tenant_id && g.tenant_id !== NIL_REQUEST_ID ? g.tenant_id : undefined,
    window: range,
  });
}

/** The groups one side's filter keeps. */
export function groupsFor(groups: UsageFailureGroup[], side: FailureSide): UsageFailureGroup[] {
  return side === "all" ? groups : groups.filter((g) => g.platform === (side === "platform"));
}

/** How many points a tile's trend line has over the page's range. */
export const TREND_POINTS = 48;

/**
 * One value per bucket from `sinceMs` on, zero where the histogram left a
 * bucket out (it returns only non-empty ones).
 */
export function trendOf<B extends { bucket_ms: number }>(buckets: B[], sinceMs: number, bucketMs: number, pick: (b: B) => number): number[] {
  if (bucketMs <= 0) return [];
  const start = Math.floor(sinceMs / bucketMs) * bucketMs;
  const out = new Array<number>(TREND_POINTS + 1).fill(0);
  for (const b of buckets) {
    const i = Math.round((b.bucket_ms - start) / bucketMs);
    if (i >= 0 && i < out.length) out[i] += pick(b);
  }
  return out;
}
