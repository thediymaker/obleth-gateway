import { obleth } from "@/lib/obleth";
import { logParamsFrom } from "@/lib/log-params";
import { liveRoute } from "@/lib/auth/guard";

// The failures among the requests matching the log's filters, split by whose
// side they were on and grouped into repeats.
export const GET = liveRoute((req) => obleth.usageFailures(logParamsFrom(req.nextUrl.searchParams)));
