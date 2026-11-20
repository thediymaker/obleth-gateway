import { obleth } from "@/lib/obleth";
import { liveRoute } from "@/lib/auth/guard";
import { numParam } from "@/lib/log-params";

export const GET = liveRoute((req) => {
  const p = req.nextUrl.searchParams;
  return obleth.usageSeriesByTenant(numParam(p, "bucket_ms", 10_000), numParam(p, "since_ms", Date.now() - 1_800_000));
});
