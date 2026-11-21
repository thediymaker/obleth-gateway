// The Management API client's transport: the admin token, request and
// error handling, streaming and query-string helpers.

import type { KeyWatchParams, UsageLogParams, UsageFilter } from "./types";

const BASE = process.env.OBLETH_ADMIN_BASE_URL ?? "http://localhost:9180";
const AUDIT_ACTOR_HEADER = "X-Obleth-Audit-Actor";

// Resolve the admin token lazily, at request time. Validating it at module
// scope would throw while Next.js evaluates server modules during `next build`
// (and in any environment without the secret), breaking builds and previews.
function adminToken(): string {
  const token = process.env.OBLETH_ADMIN_TOKEN;
  if (!token) {
    throw new Error(
      "OBLETH_ADMIN_TOKEN is not set. The control plane requires the management API admin token to operate.",
    );
  }
  return token;
}

export const keyWatchQuery = (p: KeyWatchParams) =>
  qs({
    since_ms: p.sinceMs,
    until_ms: p.untilMs,
    tenant_id: p.tenantId,
    key_id: p.keyId,
    limit: p.limit,
    per_key: p.perKey,
  });

export function usageQs(filter: UsageFilter) {
  return qs({ since_ms: filter.sinceMs, until_ms: filter.untilMs, tenant_id: filter.tenantId, model: filter.model });
}

/// Error thrown when the management API responds with a non-2xx status. Carries
/// the parsed `error` message so the UI can display something actionable.
export class OblethApiError extends Error {
  constructor(
    public readonly status: number,
    message: string,
    public readonly path: string,
  ) {
    super(message);
    this.name = "OblethApiError";
  }
}

/** Next.js fetch caching options accepted alongside a standard RequestInit. */
type NextFetchOptions = { revalidate?: number | false; tags?: string[] };
type ApiInit = RequestInit & {
  next?: NextFetchOptions;
  /** Overrides the default time limit (see `API_TIMEOUT_MS`). */
  timeoutMs?: number;
  /** Sees the response's headers (a page's next cursor). */
  onHeaders?: (headers: Headers) => void;
};

/**
 * How long the dashboard waits for the Management API before it gives up, so
 * a stalled gateway shows a fallback or an error instead of hanging a page.
 * Reads get just over the gateway's own 30 s cap on a ClickHouse read
 * (`READ_MAX_EXECUTION_SECS`), so no read the gateway allows is cut short;
 * writes run checks and side effects; `long` is for operations that walk
 * every model or move a whole configuration.
 */
export const API_TIMEOUT_MS = { read: 35_000, write: 120_000, long: 600_000 } as const;
export interface AuditOptions {
  auditActor?: string | null;
}

export function auditActorHeaders(options?: AuditOptions): Record<string, string> {
  const rawActor = options?.auditActor?.trim();
  const actor = rawActor?.replace(/[\r\n]+/g, " ");
  return actor ? { [AUDIT_ACTOR_HEADER]: actor } : {};
}

export async function api<T>(path: string, init?: ApiInit): Promise<T> {
  const { next, cache, headers, timeoutMs, onHeaders, ...rest } = init ?? {};
  const limit =
    timeoutMs ??
    ((rest.method ?? "GET").toUpperCase() === "GET" ? API_TIMEOUT_MS.read : API_TIMEOUT_MS.write);
  const timeout = AbortSignal.timeout(limit);
  const fetchInit: ApiInit = {
    ...rest,
    signal: timeout,
    headers: {
      Authorization: `Bearer ${adminToken()}`,
      "Content-Type": "application/json",
      ...(headers ?? {}),
    },
  };
  // Default to `no-store` so reads are always fresh: each dashboard replica has
  // its own Data Cache, so gateway data cached on one replica would go stale
  // on the others after a save. Only the version check opts into the cache.
  if (next) fetchInit.next = next;
  else fetchInit.cache = cache ?? "no-store";

  try {
    return await send<T>(path, fetchInit, onHeaders);
  } catch (e) {
    // The time limit reads as the gateway not answering, which `safe()`
    // turns into its fallback.
    if (timeout.aborted) {
      throw new OblethApiError(
        504,
        `The gateway did not answer in ${Math.round(limit / 1000)} s.`,
        path,
      );
    }
    throw e;
  }
}

/** One Management API call: the response body, or an `OblethApiError`. */
async function send<T>(path: string, fetchInit: ApiInit, onHeaders?: (headers: Headers) => void): Promise<T> {
  const res = await fetch(`${BASE}/api/v1${path}`, fetchInit);
  if (!res.ok) throw await apiError(res, path);
  onHeaders?.(res.headers);
  if (res.status === 204) return undefined as T;
  return (await res.json()) as T;
}

/** A non-2xx Management API response as an `OblethApiError`. */
async function apiError(res: Response, path: string): Promise<OblethApiError> {
  const text = await res.text().catch(() => "");
  // The management API returns errors as `{"error": "..."}`. Surface that
  // message directly so callers (and the UI) get something actionable instead
  // of a raw status dump.
  let message = text;
  try {
    const parsed = JSON.parse(text);
    if (parsed && typeof parsed.error === "string") message = parsed.error;
  } catch {
    // Non-JSON body; fall back to the raw text.
  }
  return new OblethApiError(
    res.status,
    message || `request failed with status ${res.status}`,
    path,
  );
}

/**
 * A Management API call whose bodies pass through untouched: a request body
 * streamed on from the browser (`duplex: "half"`), and the response returned
 * as is for the caller to stream back. Nothing is parsed or copied, so a large
 * upload or export never sits whole in the dashboard's memory. The caller sets
 * Content-Type, and can pass a `signal` (the browser's request) that stops the
 * call. A non-2xx answer throws `OblethApiError`, as `api` does; the time
 * limit also covers reading the response body.
 */
export async function apiStream(
  path: string,
  init: Omit<ApiInit, "next" | "cache"> & { duplex?: "half" },
): Promise<Response> {
  const { headers, timeoutMs, signal, ...rest } = init;
  const limit =
    timeoutMs ??
    ((rest.method ?? "GET").toUpperCase() === "GET" ? API_TIMEOUT_MS.read : API_TIMEOUT_MS.write);
  const timeout = AbortSignal.timeout(limit);
  try {
    const res = await fetch(`${BASE}/api/v1${path}`, {
      ...rest,
      cache: "no-store",
      // The caller's signal (the browser's request) stops it too.
      signal: signal ? AbortSignal.any([timeout, signal]) : timeout,
      headers: { Authorization: `Bearer ${adminToken()}`, ...(headers ?? {}) },
    } as RequestInit);
    if (!res.ok) throw await apiError(res, path);
    return res;
  } catch (e) {
    if (timeout.aborted) {
      throw new OblethApiError(
        504,
        `The gateway did not answer in ${Math.round(limit / 1000)} s.`,
        path,
      );
    }
    throw e;
  }
}

/**
 * One segment of a Management API path, from an id or a name the dashboard was
 * given (a route parameter, a form field, a tool argument). The value is
 * percent-encoded, so a `/`, `?` or `#` in it stays inside the segment, and the
 * dot segments a URL parser would resolve are refused. Without this a crafted
 * id reached a different admin endpoint than the one the caller was checked for.
 */
export function seg(value: string | number): string {
  const text = String(value);
  if (text === "" || text === "." || text === "..") {
    throw new Error("That is not a valid id.");
  }
  return encodeURIComponent(text);
}

export function qs(params: Record<string, string | number | undefined>) {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v !== undefined) q.set(k, String(v));
  }
  const s = q.toString();
  return s ? `?${s}` : "";
}

/** The request log's filters as query parameters, shared by the log, its histogram, facets and failure groups. */
export function logFilterQuery(params: UsageLogParams) {
  return {
    tenant_id: params.tenantId,
    key_id: params.keyId,
    model: params.model,
    request_type: params.requestType,
    session_id: params.sessionId,
    status: params.status,
    status_code: params.statusCode,
    fault: params.fault,
    error_class: params.errorClass,
    request_id: params.requestId,
    since_ms: params.sinceMs,
    until_ms: params.untilMs,
    traced_only: params.tracedOnly ? "true" : undefined,
    include_internal: params.includeInternal ? "true" : undefined,
  };
}
