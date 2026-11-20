import { obleth } from "@/lib/obleth";
import { logParamsFrom } from "@/lib/log-params";
import { liveRoute } from "@/lib/auth/guard";

// What the request log's matching requests are made of, with the log's filters.
export const GET = liveRoute((req) => obleth.usageLogFacets(logParamsFrom(req.nextUrl.searchParams)));
