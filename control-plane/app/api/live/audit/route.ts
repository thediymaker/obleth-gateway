import { obleth } from "@/lib/obleth";
import { liveRoute } from "@/lib/auth/guard";
import { numParam, strParam } from "@/lib/log-params";

// The audit log, narrowed by the page's filters; `before_id` pages back.
export const GET = liveRoute(async (req) => {
  const p = req.nextUrl.searchParams;
  return obleth.auditQuery({
    limit: numParam(p, "limit"),
    actor: strParam(p, "actor"),
    entityType: strParam(p, "entity_type"),
    entityId: strParam(p, "entity_id"),
    action: strParam(p, "action"),
    since: strParam(p, "since"),
    until: strParam(p, "until"),
    beforeId: numParam(p, "before_id"),
    q: strParam(p, "q"),
  });
});
