import { obleth } from "@/lib/obleth";
import { liveRoute, RouteError } from "@/lib/auth/guard";
import { numParam, strParam } from "@/lib/log-params";

export const GET = liveRoute((req) => {
  const p = req.nextUrl.searchParams;
  const model = strParam(p, "model");
  if (!model) throw new RouteError(400, "model is required");
  return obleth.usageBreakdownByModel(model, numParam(p, "since_ms", Date.now() - 3_600_000), numParam(p, "limit"));
});
