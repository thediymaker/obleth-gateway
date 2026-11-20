import { obleth } from "@/lib/obleth";
import { logParamsFrom } from "@/lib/log-params";
import { liveRoute } from "@/lib/auth/guard";

// Live request-log feed for the dashboard. Forwards the supported filters and
// keyset cursor straight through to the management API, which returns the page
// newest-first with tenant/key names already resolved.
export const GET = liveRoute((req) => obleth.usageLogs(logParamsFrom(req.nextUrl.searchParams)));
