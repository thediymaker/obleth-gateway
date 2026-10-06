import type { SpanEntry, UsageLogEntry } from "@/lib/obleth";

/**
 * The Request logs page's pure logic: filters and the search box's tokens,
 * the query string both reads share, the histogram's gaps, a row's time
 * split, and the one-line account of a request.
 */

export const LOG_WINDOWS = [
  { id: "15m", label: "15m", ms: 15 * 60_000, bucketMs: 15_000 },
  { id: "1h", label: "1h", ms: 3_600_000, bucketMs: 60_000 },
  { id: "24h", label: "24h", ms: 86_400_000, bucketMs: 1_800_000 },
  { id: "7d", label: "7d", ms: 7 * 86_400_000, bucketMs: 6 * 3_600_000 },
  { id: "30d", label: "30d", ms: 30 * 86_400_000, bucketMs: 86_400_000 },
] as const;

export type LogWindowId = (typeof LOG_WINDOWS)[number]["id"];

export const REQUEST_TYPES = ["chat", "completion", "responses", "embedding", "audio", "image", "video", "rerank", "moderation", "search", "other"] as const;

export interface LogFilters {
  /** A preset window, or `custom` with `sinceMs`/`untilMs` (a drag on the chart). */
  window: LogWindowId | "custom";
  sinceMs?: number;
  untilMs?: number;
  status: "" | "success" | "error";
  /** One exact HTTP status, as text ("" for any). */
  statusCode: string;
  model: string;
  tenantId: string;
  keyId: string;
  requestType: string;
  sessionId: string;
  /** Request id prefix, from the search box's free text. */
  requestId: string;
  tracedOnly: boolean;
  includeInternal: boolean;
}

export const DEFAULT_LOG_FILTERS: LogFilters = {
  window: "1h",
  status: "",
  statusCode: "",
  model: "",
  tenantId: "",
  keyId: "",
  requestType: "",
  sessionId: "",
  requestId: "",
  tracedOnly: false,
  includeInternal: false,
};

export function windowOf(id: LogWindowId) {
  return LOG_WINDOWS.find((w) => w.id === id) ?? LOG_WINDOWS[1];
}

/** The window the filters cover, as absolute millis at `now`. */
export function windowRange(f: LogFilters, now = Date.now()): { since: number; until: number } {
  if (f.window === "custom" && f.sinceMs != null) return { since: f.sinceMs, until: f.untilMs ?? now };
  return { since: now - windowOf(f.window === "custom" ? "1h" : f.window).ms, until: now };
}

/** A histogram bucket width that gives the window 40 to 120 bars. */
export function bucketFor(f: LogFilters, now = Date.now()): number {
  if (f.window !== "custom") return windowOf(f.window).bucketMs;
  const { since, until } = windowRange(f, now);
  const steps = [1_000, 5_000, 15_000, 60_000, 300_000, 900_000, 1_800_000, 3_600_000, 6 * 3_600_000, 86_400_000];
  return steps.find((s) => (until - since) / s <= 120) ?? 86_400_000;
}

/** The query both the list and the histogram send; the cursor is the list's. */
export function logParams(f: LogFilters, range: { since: number; until: number }, extra: Record<string, string | number | undefined> = {}): URLSearchParams {
  const p = new URLSearchParams();
  p.set("since_ms", String(Math.floor(range.since)));
  if (f.window === "custom") p.set("until_ms", String(Math.floor(range.until)));
  if (f.status) p.set("status", f.status);
  if (f.statusCode) p.set("status_code", f.statusCode);
  if (f.model) p.set("model", f.model);
  if (f.tenantId) p.set("tenant_id", f.tenantId);
  if (f.keyId) p.set("key_id", f.keyId);
  if (f.requestType) p.set("request_type", f.requestType);
  if (f.sessionId) p.set("session_id", f.sessionId);
  if (f.requestId) p.set("request_id", f.requestId);
  if (f.tracedOnly) p.set("traced_only", "true");
  if (f.includeInternal) p.set("include_internal", "true");
  for (const [k, v] of Object.entries(extra)) if (v !== undefined && v !== "") p.set(k, String(v));
  return p;
}

export function filtersActive(f: LogFilters): boolean {
  return !!(f.status || f.statusCode || f.model || f.tenantId || f.keyId || f.requestType || f.sessionId || f.requestId || f.tracedOnly || f.includeInternal || f.window === "custom");
}

// ---------------------------------------------------------------------------
// The search box
// ---------------------------------------------------------------------------

export interface Named { id: string; name: string }
export interface KeyOption { id: string; name: string; prefix: string; tenantId: string }

export interface SearchLookups {
  tenants: Named[];
  keys: KeyOption[];
  models: string[];
}

/**
 * Turn `status:error model:glm-5-3 team:cs-teaching 7c1e` into filters.
 * Teams and keys resolve by name (or key prefix), case-insensitively; a name
 * that matches nothing comes back in `unknown` so the box can say so. Free
 * text is a request-id prefix.
 */
export function parseSearch(text: string, lookups: SearchLookups): { patch: Partial<LogFilters>; unknown: string[] } {
  const patch: Partial<LogFilters> = {};
  const unknown: string[] = [];
  const free: string[] = [];
  const lower = (s: string) => s.trim().toLowerCase();
  for (const token of text.trim().split(/\s+/).filter(Boolean)) {
    const colon = token.indexOf(":");
    const key = colon > 0 ? token.slice(0, colon).toLowerCase() : "";
    const value = colon > 0 ? token.slice(colon + 1) : token;
    if (!key || !value) {
      free.push(token);
      continue;
    }
    if (key === "status") {
      const v = lower(value);
      if (/^[1-5]\d\d$/.test(v)) patch.statusCode = v;
      else if (["error", "failed", "fail"].includes(v)) patch.status = "error";
      else if (["ok", "success", "succeeded"].includes(v)) patch.status = "success";
      else unknown.push(token);
    } else if (key === "model") {
      const m = lookups.models.find((x) => lower(x) === lower(value));
      if (m) patch.model = m;
      else unknown.push(token);
    } else if (key === "team" || key === "tenant") {
      const t = lookups.tenants.find((x) => lower(x.name) === lower(value));
      if (t) patch.tenantId = t.id;
      else unknown.push(token);
    } else if (key === "key") {
      const k = lookups.keys.find((x) => lower(x.prefix) === lower(value) || lower(x.name) === lower(value));
      if (k) {
        patch.keyId = k.id;
        patch.tenantId = k.tenantId;
      } else unknown.push(token);
    } else if (key === "type") {
      const t = REQUEST_TYPES.find((x) => x === lower(value));
      if (t) patch.requestType = t;
      else unknown.push(token);
    } else if (key === "session") {
      patch.sessionId = value;
    } else if (key === "id" || key === "request") {
      free.push(value);
    } else {
      unknown.push(token);
    }
  }
  if (free.length) patch.requestId = free.join("");
  return { patch, unknown };
}

// ---------------------------------------------------------------------------
// The histogram
// ---------------------------------------------------------------------------

export interface HistogramBucket { bucket_ms: number; requests: number; errors: number }

/** Every bucket across the window, the missing ones as zero, oldest first. */
export function fillHistogram(buckets: HistogramBucket[], range: { since: number; until: number }, bucketMs: number): HistogramBucket[] {
  const byT = new Map(buckets.map((b) => [b.bucket_ms, b]));
  const out: HistogramBucket[] = [];
  const first = Math.floor(range.since / bucketMs) * bucketMs;
  for (let t = first; t <= range.until; t += bucketMs) {
    out.push(byT.get(t) ?? { bucket_ms: t, requests: 0, errors: 0 });
    if (out.length > 1000) break;
  }
  return out;
}

// ---------------------------------------------------------------------------
// A row
// ---------------------------------------------------------------------------

export const isFailure = (row: Pick<UsageLogEntry, "status_code">) => row.status_code >= 400;

/**
 * A request's time as wait for a slot, then up to the first token, then the
 * rest. The first token is measured from arrival, so it includes the wait; a
 * non-streamed reply's first token is its last.
 */
export function timeSplit(row: Pick<UsageLogEntry, "queue_wait_ms" | "ttft_ms" | "total_ms">) {
  const total = Math.max(row.total_ms, 0);
  const wait = Math.min(Math.max(row.queue_wait_ms, 0), total);
  const first = row.ttft_ms > 0 ? Math.min(Math.max(row.ttft_ms - wait, 0), total - wait) : 0;
  const rest = Math.max(total - wait - first, 0);
  return { wait, first, rest, total };
}

/** "12 s ago", "4 min ago", then the clock time for anything older than an hour. */
export function when(ts: number, now = Date.now()): string {
  const s = Math.max(0, Math.round((now - ts) / 1000));
  if (s < 60) return `${s} s ago`;
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  const d = new Date(ts);
  const sameDay = new Date(now).toDateString() === d.toDateString();
  return sameDay
    ? d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
    : d.toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

export function duration(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return "0 ms";
  if (ms < 1000) return `${Math.round(ms)} ms`;
  if (ms < 60_000) return `${+(ms / 1000).toFixed(ms < 10_000 ? 2 : 1)} s`;
  return `${+(ms / 60_000).toFixed(1)} min`;
}

/** A bar's width as people say it: "15 s", "minute", "30 min", "6 h", "day". */
export function bucketLabel(ms: number): string {
  if (ms === 60_000) return "minute";
  if (ms === 3_600_000) return "hour";
  if (ms === 86_400_000) return "day";
  if (ms < 60_000) return `${Math.round(ms / 1000)} s`;
  if (ms < 3_600_000) return `${Math.round(ms / 60_000)} min`;
  if (ms < 86_400_000) return `${Math.round(ms / 3_600_000)} h`;
  return `${Math.round(ms / 86_400_000)} days`;
}

export function tokens(n: number): string {
  if (n >= 1e6) return `${+(n / 1e6).toFixed(1)}M`;
  if (n >= 1e3) return `${+(n / 1e3).toFixed(1)}K`;
  return String(n);
}

export function cost(usd: number): string {
  if (!usd) return "$0";
  if (usd < 0.0001) return "< $0.0001";
  return `$${usd < 1 ? usd.toFixed(4) : usd.toFixed(2)}`;
}

// ---------------------------------------------------------------------------
// Helper calls: a boon's own calls to other models
// ---------------------------------------------------------------------------

/** The id a row carries when it served no other request. */
export const NIL_REQUEST_ID = "00000000-0000-0000-0000-000000000000";

const HELPER_PURPOSES: Record<string, string> = {
  speculation_draft: "speculation draft",
  speculation_verify: "draft check",
  vision_boon: "image description",
  structured_output_boon: "structured output repair",
  guardrails_boon: "guardrails scan",
  tool_loop: "tool loop turn",
  image_generation_boon: "image generation",
  web_search_boon: "web search",
};

/** What a helper call was for, in a few words; any other type as recorded. */
export function helperPurpose(requestType: string): string {
  return HELPER_PURPOSES[requestType] ?? (requestType || "other");
}

/** The client request a helper call served, or null for a request a client made. */
export function servedRequest(row: Pick<UsageLogEntry, "parent_request_id">): string | null {
  const id = row.parent_request_id ?? "";
  return id && id !== NIL_REQUEST_ID ? id : null;
}

/**
 * The query for a request's helper calls. They are written while it runs and
 * now and then just after it ends (`ts_ms` is when it ended), so an hour
 * either side finds them all. Health checks and benchmarks have helpers too.
 */
export function helperCallsParams(row: Pick<UsageLogEntry, "request_id" | "ts_ms">): URLSearchParams {
  return new URLSearchParams({
    parent_request_id: row.request_id,
    since_ms: String(row.ts_ms - 3_600_000),
    until_ms: String(row.ts_ms + 3_600_000),
    include_internal: "true",
    limit: "200",
  });
}

export const HTTP_REASONS: Record<number, string> = {
  400: "Bad request",
  401: "Unauthorized",
  403: "Forbidden",
  404: "Not found",
  408: "Timed out",
  413: "Too large",
  422: "Unprocessable",
  429: "Too many requests",
  499: "Client went away",
  500: "Gateway error",
  502: "Bad gateway",
  503: "Unavailable",
  504: "Upstream timed out",
};

// ---------------------------------------------------------------------------
// The one-line account
// ---------------------------------------------------------------------------

export function parseSpanAttrs(raw: string): Record<string, unknown> {
  try {
    const v = JSON.parse(raw);
    return v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : {};
  } catch {
    return {};
  }
}

/**
 * What happened to a request, in a sentence or three, from its log row and,
 * when it was traced, its spans. Says only what those record.
 */
export function describeRequest(row: UsageLogEntry, spans: SpanEntry[] = []): string {
  const find = (name: string) => spans.find((s) => s.span_name === name);
  const parts: string[] = [];
  const admission = find("admission");
  const decision = admission ? String(parseSpanAttrs(admission.attributes).decision ?? row.admission) : row.admission;
  // The gateway records `fast`, `queued`, `rejected`, or `boon` for a call
  // it made itself on a boon's behalf.
  if (decision === "rejected") parts.push("Turned away at admission.");
  else if (decision === "boon") parts.push("A call the gateway made itself, for a boon.");
  else if (row.queue_wait_ms > 0) parts.push(`Waited ${duration(row.queue_wait_ms)} for a slot in the ${row.model} pool.`);
  else if (decision) parts.push("Admitted straight away.");

  const route = find("auto_route");
  if (route) {
    const chosen = parseSpanAttrs(route.attributes).chosen;
    if (typeof chosen === "string" && chosen) parts.push(`The auto router picked ${chosen}.`);
  }
  if (row.cache_status === "hit") {
    parts.push("Answered from the response cache, without calling the upstream.");
  } else {
    const up = find("upstream") ?? find("dispatch");
    const upAttrs = up ? parseSpanAttrs(up.attributes) : {};
    const targets = Number(upAttrs.targets ?? 0);
    const tried = targets > 1 ? `, with ${targets} endpoints to try` : "";
    if (isFailure(row)) {
      const who = up ? "The upstream" : "The gateway";
      parts.push(`${who} answered ${row.status_code}${HTTP_REASONS[row.status_code] ? ` (${HTTP_REASONS[row.status_code]})` : ""} after ${duration(row.total_ms)}${tried}.`);
    } else if (row.output_tokens > 0 && row.ttft_ms > 0 && row.total_ms - row.ttft_ms > 50) {
      parts.push(`First token after ${duration(row.ttft_ms)}, finished at ${duration(row.total_ms)}${tried}.`);
    } else {
      parts.push(`Answered in ${duration(row.total_ms)}${tried}.`);
    }
  }
  if (isFailure(row) && !row.cost_usd) parts.push("Nothing was billed.");
  return parts.join(" ");
}

// ---------------------------------------------------------------------------
// Saved views
// ---------------------------------------------------------------------------

export interface SavedView { name: string; filters: LogFilters }

const VIEWS_KEY = "obleth:request-logs:views";

/** Views live in this browser only. Storage can be missing or full; both read as none saved. */
export function loadViews(): SavedView[] {
  try {
    const raw = JSON.parse(localStorage.getItem(VIEWS_KEY) ?? "[]");
    return Array.isArray(raw) ? raw.filter((v) => v && typeof v.name === "string" && v.filters).map((v) => ({ name: v.name, filters: { ...DEFAULT_LOG_FILTERS, ...v.filters } })) : [];
  } catch {
    return [];
  }
}

export function storeViews(views: SavedView[]): boolean {
  try {
    localStorage.setItem(VIEWS_KEY, JSON.stringify(views.slice(0, 30)));
    return true;
  } catch {
    return false;
  }
}
