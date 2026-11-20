import { obleth, type UsageSpeedParams } from "@/lib/obleth";
import { liveRoute } from "@/lib/auth/guard";
import { numParam, strParam } from "@/lib/log-params";

export const GET = liveRoute((req) => {
  const p = req.nextUrl.searchParams;
  const untilMs = numParam(p, "until_ms", Date.now());
  return obleth.usageSpeed({
    sinceMs: numParam(p, "since_ms", untilMs - 86_400_000),
    untilMs,
    bucket: strParam(p, "bucket") as UsageSpeedParams["bucket"] | undefined,
    tz: strParam(p, "tz"),
    tenantId: strParam(p, "tenant_id"),
    keyId: strParam(p, "key_id"),
    model: strParam(p, "model"),
  });
});
