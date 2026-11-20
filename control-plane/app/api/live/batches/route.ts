import { obleth } from "@/lib/obleth";
import { liveRoute } from "@/lib/auth/guard";
import { numParam, strParam } from "@/lib/log-params";

// Batches across every tenant for the Batches page's polling; `status`,
// `tenant_id`, `model` and `after` narrow and page the list.
export const GET = liveRoute(async (req) => {
  const p = req.nextUrl.searchParams;
  return obleth.listBatches({
    status: strParam(p, "status"),
    tenantId: strParam(p, "tenant_id"),
    model: strParam(p, "model"),
    after: strParam(p, "after"),
    limit: numParam(p, "limit"),
  });
});
