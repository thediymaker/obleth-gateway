import { obleth } from "@/lib/obleth";
import { liveRoute } from "@/lib/auth/guard";
import { numParam, strParam } from "@/lib/log-params";

export const GET = liveRoute((req) => {
  const p = req.nextUrl.searchParams;
  return obleth.fairshareHistory({ since_ms: numParam(p, "since_ms"), model: strParam(p, "model") });
});
