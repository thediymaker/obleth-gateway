import { obleth, type UsageDailyGroupBy } from "@/lib/obleth";
import { liveRoute } from "@/lib/auth/guard";
import { strParam, utcDay } from "@/lib/log-params";

// The daily rollup; the last seven UTC days unless `start_day`/`end_day` say otherwise.
export const GET = liveRoute((req) => {
  const p = req.nextUrl.searchParams;
  return obleth.usageDaily({
    startDay: strParam(p, "start_day") ?? utcDay(7),
    endDay: strParam(p, "end_day") ?? utcDay(),
    groupBy: strParam(p, "group_by") as UsageDailyGroupBy | undefined,
    tenantId: strParam(p, "tenant_id"),
    keyId: strParam(p, "key_id"),
    model: strParam(p, "model"),
  });
});
