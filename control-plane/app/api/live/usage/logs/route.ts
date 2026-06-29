import { NextRequest, NextResponse } from "next/server";
import { obleth, type UsageLogParams, type UsageLogStatus } from "@/lib/obleth";

// Live request-log feed for the dashboard. Forwards the supported filters and
// keyset cursor straight through to the management API, which returns the page
// newest-first with tenant/key names already resolved.
export async function GET(req: NextRequest) {
  const sp = req.nextUrl.searchParams;
  const str = (k: string) => sp.get(k)?.trim() || undefined;
  const num = (k: string) => {
    const v = sp.get(k);
    return v !== null && v !== "" && Number.isFinite(Number(v)) ? Number(v) : undefined;
  };
  const statusRaw = str("status");
  const status: UsageLogStatus | undefined =
    statusRaw === "success" || statusRaw === "error" ? statusRaw : undefined;

  const params: UsageLogParams = {
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
    includeInternal: sp.get("include_internal") === "true" ? true : undefined,
  };

  try {
    const logs = await obleth.usageLogs(params);
    return NextResponse.json(logs);
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
