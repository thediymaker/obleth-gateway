import { obleth } from "@/lib/obleth";
import { liveRoute } from "@/lib/auth/guard";
import { numParam } from "@/lib/log-params";

export const GET = liveRoute((req) => obleth.usageByModel({ sinceMs: numParam(req.nextUrl.searchParams, "since_ms", Date.now() - 3_600_000) }));
