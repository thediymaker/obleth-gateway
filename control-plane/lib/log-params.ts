import type { FaultSide, UsageLogParams, UsageLogStatus } from "@/lib/obleth";

/** A query parameter's text, trimmed; absent or blank is `undefined`. */
export function strParam(sp: URLSearchParams, name: string): string | undefined {
  return sp.get(name)?.trim() || undefined;
}

/** A query parameter as a finite number, so NaN never reaches the gateway; absent, blank or not a number is `fallback`. */
export function numParam(sp: URLSearchParams, name: string): number | undefined;
export function numParam(sp: URLSearchParams, name: string, fallback: number): number;
export function numParam(sp: URLSearchParams, name: string, fallback?: number): number | undefined {
  const v = sp.get(name)?.trim();
  return v && Number.isFinite(Number(v)) ? Number(v) : fallback;
}

/** A UTC day, `YYYY-MM-DD`, `daysAgo` days before now. */
export function utcDay(daysAgo = 0, now = Date.now()): string {
  return new Date(now - daysAgo * 86_400_000).toISOString().slice(0, 10);
}

/** The log's filters and cursor from a request's query string. */
export function logParamsFrom(sp: URLSearchParams): UsageLogParams {
  const str = (k: string) => strParam(sp, k);
  const num = (k: string) => numParam(sp, k);
  const statusRaw = str("status");
  const status: UsageLogStatus | undefined =
    statusRaw === "success" || statusRaw === "error" ? statusRaw : undefined;
  const faultRaw = str("fault");
  const fault: FaultSide | undefined = faultRaw === "platform" || faultRaw === "client" ? faultRaw : undefined;

  return {
    tenantId: str("tenant_id"),
    keyId: str("key_id"),
    model: str("model"),
    requestType: str("request_type"),
    sessionId: str("session_id"),
    status,
    statusCode: num("status_code"),
    fault,
    errorClass: str("error_class"),
    requestId: str("request_id"),
    sinceMs: num("since_ms"),
    untilMs: num("until_ms"),
    beforeMs: num("before_ms"),
    beforeRequestId: str("before_request_id"),
    limit: num("limit"),
    tracedOnly: sp.get("traced_only") === "true" ? true : undefined,
    includeInternal: sp.get("include_internal") === "true" ? true : undefined,
    parentRequestId: str("parent_request_id"),
  };
}
