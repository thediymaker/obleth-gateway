import { obleth } from "@/lib/obleth";
import { logParamsFrom, numParam } from "@/lib/log-params";
import { liveRoute } from "@/lib/auth/guard";

// Requests and failures over time for the request log, with the log's filters.
export const GET = liveRoute((req) => {
  const sp = req.nextUrl.searchParams;
  const bucket = numParam(sp, "bucket_ms");
  return obleth.usageLogHistogram({ ...logParamsFrom(sp), bucketMs: bucket && bucket > 0 ? bucket : undefined });
});
