import type { UsageLogParams, UsageLogStatus } from "@/lib/obleth";

/** The log's filters and cursor from a request's query string. */
export function logParamsFrom(sp: URLSearchParams): UsageLogParams {
  const str = (k: string) => sp.get(k)?.trim() || undefined;
  const num = (k: string) => {
    const v = sp.get(k);
    return v !== null && v !== "" && Number.isFinite(Number(v)) ? Number(v) : undefined;
  };
  const statusRaw = str("status");
  const status: UsageLogStatus | undefined =
    statusRaw === "success" || statusRaw === "error" ? statusRaw : undefined;

  return {
    tenantId: str("tenant_id"),
    keyId: str("key_id"),
    model: str("model"),
    requestType: str("request_type"),
    sessionId: str("session_id"),
    status,
    requestId: str("request_id"),
    sinceMs: num("since_ms"),
    untilMs: num("until_ms"),
    beforeMs: num("before_ms"),
    beforeRequestId: str("before_request_id"),
    limit: num("limit"),
    tracedOnly: sp.get("traced_only") === "true" ? true : undefined,
    includeInternal: sp.get("include_internal") === "true" ? true : undefined,
  };
}
