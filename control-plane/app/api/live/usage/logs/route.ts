import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { logParamsFrom } from "@/lib/log-params";
import { guardAdmin } from "@/lib/auth/guard";

// Live request-log feed for the dashboard. Forwards the supported filters and
// keyset cursor straight through to the management API, which returns the page
// newest-first with tenant/key names already resolved.
export async function GET(req: NextRequest) {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    const logs = await obleth.usageLogs(logParamsFrom(req.nextUrl.searchParams));
    return NextResponse.json(logs);
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
